// SPDX-License-Identifier: Apache-2.0
#include "decoder.hpp"
#include "logic.hpp"
#include "skylatch_client_core.h"

#define SDL_MAIN_HANDLED
#include <SDL.h>
#include <json_tokener.h>
extern "C" {
#include <libavutil/frame.h>
}

#include <algorithm>
#include <atomic>
#include <deque>
#include <charconv>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <iostream>
#include <limits>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <thread>
#include <utility>
#include <variant>
#include <unistd.h>

namespace skylatch {
namespace {
using namespace std::chrono_literals;
using Clock = std::chrono::steady_clock;
using ClientPtr = std::unique_ptr<SkylatchClient, decltype(&skylatch_destroy)>;
constexpr size_t maxClipboard = 1024 * 1024;

static_assert(SDL_SCANCODE_UNKNOWN == 0 && SDL_SCANCODE_A == 4 && SDL_SCANCODE_Z == 29 &&
              SDL_SCANCODE_1 == 30 && SDL_SCANCODE_0 == 39 && SDL_SCANCODE_RETURN == 40 &&
              SDL_SCANCODE_F1 == 58 && SDL_SCANCODE_F11 == 68 && SDL_SCANCODE_F12 == 69 &&
              SDL_SCANCODE_KP_0 == 98 && SDL_SCANCODE_NONUSBACKSLASH == 100 &&
              SDL_SCANCODE_F13 == 104 && SDL_SCANCODE_F24 == 115 && SDL_SCANCODE_VOLUMEDOWN == 129 &&
              SDL_SCANCODE_KP_COMMA == 133 && SDL_SCANCODE_INTERNATIONAL1 == 135 &&
              SDL_SCANCODE_LANG9 == 152 && SDL_SCANCODE_EXSEL == 164 && SDL_SCANCODE_KP_00 == 176 &&
              SDL_SCANCODE_KP_HEXADECIMAL == 221 && SDL_SCANCODE_LCTRL == 224 && SDL_SCANCODE_LSHIFT == 225 &&
              SDL_SCANCODE_LALT == 226 && SDL_SCANCODE_LGUI == 227 && SDL_SCANCODE_RCTRL == 228 &&
              SDL_SCANCODE_RSHIFT == 229 && SDL_SCANCODE_RALT == 230 && SDL_SCANCODE_RGUI == 231 &&
              SDL_SCANCODE_MODE == 257 && SDL_SCANCODE_AUDIONEXT == 258);

struct Device { std::string id, name, os, monitors, endpoints; };

std::vector<Device> discover(const Config& config) {
    char error[512]{};
    const std::unique_ptr<char, decltype(&skylatch_free_string)> raw(
        skylatch_list_devices_json(config.hub.c_str(), config.token.c_str(), error, sizeof(error)),
        skylatch_free_string);
    if (!raw) throw std::runtime_error(*error ? terminalText(error) : "hub discovery failed");
    const std::unique_ptr<json_object, decltype(&json_object_put)> json(json_tokener_parse(raw.get()), json_object_put);
    const auto field = [](json_object* obj, const char* name, json_type type) {
        json_object* value{};
        if (!json_object_object_get_ex(obj, name, &value) || !json_object_is_type(value, type))
            throw std::runtime_error("invalid hub device list");
        return value;
    };
    const auto text = [&](json_object* obj, const char* name) {
        auto* s = field(obj, name, json_type_string);
        return std::string(json_object_get_string(s), json_object_get_string_len(s));
    };
    if (!json_object_is_type(json.get(), json_type_array)) throw std::runtime_error("invalid hub device list");
    std::vector<Device> devices;
    for (size_t i = 0; i < json_object_array_length(json.get()); ++i) {
        auto* obj = json_object_array_get_idx(json.get(), i);
        Device device{text(obj, "device_id"), terminalText(text(obj, "name")), terminalText(text(obj, "os")),
                      std::to_string(json_object_get_int64(field(obj, "monitors", json_type_int))), {}};
        if (device.id.empty() || device.id.find('\0') != device.id.npos)
            throw std::runtime_error("hub returned an invalid device ID");
        auto* endpoints = field(obj, "direct_endpoints", json_type_array);
        for (size_t j = 0; j < json_object_array_length(endpoints); ++j) {
            auto* endpoint = json_object_array_get_idx(endpoints, j);
            auto host = terminalText(text(endpoint, "host"));
            if (host.find(':') != host.npos && !host.starts_with('[')) host = '[' + host + ']';
            if (j) device.endpoints += ", ";
            device.endpoints += terminalText(text(endpoint, "kind")) + ':' + host + ':' +
                std::to_string(json_object_get_int64(field(endpoint, "port", json_type_int)));
        }
        if (device.endpoints.empty()) device.endpoints = "-";
        devices.push_back(std::move(device));
    }
    return devices;
}

bool selectDevice(Config& config) {
    if (config.hub.empty() || !config.device.empty()) return true;
    const auto devices = discover(config);
    if (config.list) {
        std::cout << "device_id\tname\tos\tmonitors\tdirect_endpoints\n";
        for (const auto& d : devices)
            std::cout << terminalText(d.id) << '\t' << d.name << '\t' << d.os << '\t' << d.monitors << '\t' << d.endpoints << '\n';
        return false;
    }
    if (devices.empty()) throw std::runtime_error("no devices are online");
    size_t choice = 1;
    if (devices.size() > 1) {
        if (config.smokeSeconds || !isatty(STDIN_FILENO))
            throw std::runtime_error("multiple devices are online; run --list and specify --device");
        for (size_t i = 0; i < devices.size(); ++i) {
            const auto& d = devices[i];
            std::cerr << i + 1 << ") " << d.name << " [" << terminalText(d.id) << "] " << d.os << '\n';
        }
        std::cerr << "Select device: " << std::flush;
        std::string line;
        if (!std::getline(std::cin, line)) throw std::runtime_error("no device selected");
        const auto [end, ec] = std::from_chars(line.data(), line.data() + line.size(), choice);
        if (ec != std::errc{} || end != line.data() + line.size() || choice < 1 || choice > devices.size())
            throw std::runtime_error("invalid device choice");
    }
    config.device = devices[choice - 1].id;
    return true;
}

struct FrameTag { uint32_t generation, sequence; Clock::time_point arrival; };
struct Frame { FrameTag tag; bool keyframe; std::vector<uint8_t> bytes; };
struct Cursor { uint16_t width, height, xhot, yhot; std::vector<uint8_t> rgba; };
struct End { std::string reason; };
using Update = std::variant<Frame, Cursor, std::string, End>;

class Mailbox {
    std::mutex mutex;
    std::condition_variable ready;
    std::deque<Update> queue;
    static constexpr size_t maxQueuedBytes = 16 * 1024 * 1024;
    size_t queuedBytes{};
    bool failed{};
    const bool display;
    std::atomic<bool> smooth{};

    template<class Fn> void receive(size_t size, Fn fn) noexcept {
        std::lock_guard lock(mutex);
        // The host's video in-flight budget is 8 MiB. Bound all copied callback payloads.
        if (size > maxQueuedBytes - queuedBytes || queue.size() >= 2048) failed = true;
        if (!failed) {
            try { fn(); queuedBytes += size; }
            catch (...) { failed = true; }
        }
        ready.notify_one();
    }

public:
    explicit Mailbox(bool show) : display(show) {}
    static void frame(void* user, uint32_t generation, uint32_t sequence, uint16_t, uint16_t, uint64_t,
                      bool keyframe, const uint8_t* data, size_t size) noexcept {
        const auto arrival = Clock::now();
        auto& self = *static_cast<Mailbox*>(user);
        self.receive(size, [&] {
            if (!size || !data) { self.failed = true; return; }
            self.queue.emplace_back(Frame{{generation, sequence, arrival}, keyframe, {data, data + size}});
        });
    }
    static void cursor(void* user, uint16_t w, uint16_t h, uint16_t x, uint16_t y,
                       const uint8_t* data, size_t size) noexcept {
        auto& self = *static_cast<Mailbox*>(user);
        if (!self.display) return;
        self.receive(size, [&] {
            Cursor value{w, h, x, y, {}};
            if (size) value.rgba.assign(data, data + size);
            self.queue.emplace_back(std::move(value));
        });
    }
    static void clipboard(void* user, const char* data, size_t size) noexcept {
        auto& self = *static_cast<Mailbox*>(user);
        self.receive(size, [&] { self.queue.emplace_back(std::string(data ? data : "", size)); });
    }
    static void inputResponse(void* user, uint32_t sequence, uint8_t status) noexcept {
        if (!sequence) static_cast<Mailbox*>(user)->smooth = status == SKYLATCH_INPUT_ACCEPTED;
    }
    bool smoothWheel() const { return smooth; }
    static void disconnected(void* user, const char* why) noexcept {
        auto& self = *static_cast<Mailbox*>(user);
        self.receive(0, [&] { self.queue.emplace_back(End{why ? terminalText(why) : ""}); });
    }
    std::optional<Update> next(std::chrono::milliseconds wait) {
        std::unique_lock lock(mutex);
        if (wait.count()) ready.wait_for(lock, wait, [&] { return failed || !queue.empty(); });
        if (failed) throw std::runtime_error("incoming stream exceeded client capacity");
        if (queue.empty()) return {};
        auto update = std::move(queue.front());
        queue.pop_front();
        if (const auto* f = std::get_if<Frame>(&update)) queuedBytes -= f->bytes.size();
        else if (const auto* c = std::get_if<Cursor>(&update)) queuedBytes -= c->rgba.size();
        else if (const auto* s = std::get_if<std::string>(&update)) queuedBytes -= s->size();
        return update;
    }
};

ClientPtr connect(const Config& config, Mailbox& mailbox) {
    char error[512]{};
    const SkylatchCallbacks callbacks{Mailbox::frame, Mailbox::cursor, nullptr, Mailbox::disconnected,
                                      Mailbox::inputResponse};
    ClientPtr client(config.hub.empty()
        ? skylatch_connect_direct(config.direct.c_str(), config.token.empty() ? nullptr : config.token.c_str(),
                                  &callbacks, &mailbox, error, sizeof(error))
        : skylatch_connect_hub(config.hub.c_str(), config.token.c_str(), config.device.c_str(), !config.noDirect,
                               &callbacks, &mailbox, error, sizeof(error)), skylatch_destroy);
    if (!client) throw std::runtime_error(*error ? terminalText(error) : "connection failed");
    if (!config.noClipboard && !config.smokeSeconds) {
        const SkylatchSl1Callbacks sl1{nullptr, Mailbox::clipboard};
        if (skylatch_set_sl1_callbacks(client.get(), &sl1, &mailbox) != SKYLATCH_OK)
            throw std::runtime_error("could not register clipboard callback");
    }
    return client;
}

void sdlCheck(bool ok, const char* operation) {
    if (!ok) throw std::runtime_error(std::string(operation) + ": " + SDL_GetError());
}

struct VideoSubsystem {
    VideoSubsystem() {
        SDL_SetMainReady();
        SDL_SetHint(SDL_HINT_RENDER_SCALE_QUALITY, "1");
        SDL_SetHint(SDL_HINT_ALLOW_ALT_TAB_WHILE_GRABBED, "0");
        SDL_SetHint(SDL_HINT_IME_SHOW_UI, "1");
#if SDL_VERSION_ATLEAST(2, 0, 22)
        SDL_SetHint(SDL_HINT_IME_SUPPORT_EXTENDED_TEXT, "1");
#endif
        sdlCheck(SDL_Init(SDL_INIT_VIDEO) == 0, "initialize SDL video");
    }
    ~VideoSubsystem() { SDL_Quit(); }
};

template<class T, void (*Free)(T*)> struct SdlFree { void operator()(T* ptr) const { Free(ptr); } };

class Viewer {
    VideoSubsystem video;
    std::unique_ptr<SDL_Window, SdlFree<SDL_Window, SDL_DestroyWindow>> window;
    std::unique_ptr<SDL_Renderer, SdlFree<SDL_Renderer, SDL_DestroyRenderer>> renderer;
    std::unique_ptr<SDL_Texture, SdlFree<SDL_Texture, SDL_DestroyTexture>> texture;
    std::unique_ptr<SDL_Cursor, SdlFree<SDL_Cursor, SDL_FreeCursor>> cursorImage;
    std::array<std::vector<uint8_t>, 3> scratch;
    SkylatchClient* client;
    const Mailbox& mailbox;
    const bool clipboardEnabled;
    WheelEncoder wheels;
    int width{}, height{};
    Uint32 textureFormat{};
    bool fullscreen{}, grabbed{}, focused{}, composing{}, commitFromComposition{}, grabKey{};
    std::array<bool, 232> held{};
    uint16_t buttons{}, modifiers{};
    struct DeferredKey { uint16_t usage; Clock::time_point expires; };
    std::optional<DeferredKey> deferred;
    std::optional<std::string> lastSyncedClipboard, pendingClipboard;

    bool send(const InputRecord& bytes, bool droppable = false) {
        for (int attempt = 0; ; ++attempt) {
            const auto rc = skylatch_send_input(client, bytes.data());
            if (rc == SKYLATCH_OK) return true;
            if (rc == SKYLATCH_ERR_NOT_READY || rc == SKYLATCH_ERR_CLOSED || rc == SKYLATCH_ERR_UNSUPPORTED) return false;
            if (rc != SKYLATCH_ERR_QUEUE_FULL || (!droppable && attempt == 20))
                throw std::runtime_error("could not send input");
            if (droppable) return false;
            std::this_thread::sleep_for(1ms);
        }
    }
    void key(uint16_t usage, bool down) {
        if (held[usage] == down) return;
        const uint16_t bit = modifierBit(usage);
        const uint16_t next = down ? modifiers | bit : modifiers & ~bit;
        if (send(inputRecord(down ? Input::keyDown : Input::keyUp, usage, 0, {}, buttons, next))) {
            held[usage] = down;
            modifiers = next;
        }
    }
    void flushKey() {
        if (auto pending = std::exchange(deferred, std::nullopt)) key(pending->usage, true);
    }
    void keyboard(const SDL_KeyboardEvent& e) {
        if (!focused) return;
        const auto scan = e.keysym.scancode;
        const auto usage = hidUsage(scan);
        if (!usage) return;
        const auto mods = e.keysym.mod;
        const bool down = e.type == SDL_KEYDOWN;
        if (scan == SDL_SCANCODE_F11) {
            if (down && !e.repeat) {
                sdlCheck(SDL_SetWindowFullscreen(window.get(), fullscreen ? 0 : SDL_WINDOW_FULLSCREEN_DESKTOP) == 0,
                         "toggle fullscreen");
                fullscreen = !fullscreen;
                redraw();
            }
            return;
        }
        if (scan == SDL_SCANCODE_G && (grabKey || (down && !e.repeat && (mods & KMOD_CTRL) && (mods & KMOD_ALT)))) {
            if (down && !grabKey) {
                grabbed = !grabbed;
                SDL_SetWindowKeyboardGrab(window.get(), grabbed ? SDL_TRUE : SDL_FALSE);
                std::cerr << "Keyboard grab " << (grabbed ? "enabled" : "disabled") << '\n';
            }
            grabKey = down;
            return;
        }
        if (e.repeat) return; // Held HID keys repeat on the host; SDL_TEXTINPUT can still repeat local commits.
        flushKey();
        if (!down) { key(usage, false); return; }
        if (composing && !modifierBit(usage)) return;
        const bool shortcut = (mods & (KMOD_GUI | KMOD_LALT)) ||
                              ((mods & KMOD_CTRL) && !(mods & (KMOD_RALT | KMOD_MODE)));
        // Printable or dead key: give SDL's corresponding text/edit event a chance to consume it.
        if (!shortcut && ((e.keysym.sym >= 32 && e.keysym.sym < SDLK_SCANCODE_MASK) ||
                          e.keysym.sym == SDLK_UNKNOWN || (usage >= 84 && usage <= 99 && usage != 88))) {
            deferred = DeferredKey{usage, Clock::now() + 8ms};
        } else key(usage, true);
    }
    void editing(bool active) {
        if (active) deferred.reset();
        commitFromComposition = composing || active;
        composing = active;
    }
    void text(const char* utf8) {
        if (!focused) return;
        const auto scalars = committedText(utf8, composing || commitFromComposition);
        commitFromComposition = composing = false;
        if (scalars.empty()) { flushKey(); return; }
        deferred.reset();
        for (const auto cp : scalars) send(inputRecord(Input::text, 0, static_cast<int32_t>(cp)));
    }
    Point pointer(int x, int y) const {
        int ww{}, wh{}, dw{}, dh{};
        SDL_GetWindowSize(window.get(), &ww, &wh);
        SDL_GetRendererOutputSize(renderer.get(), &dw, &dh);
        return mapPointer(letterbox(dw, dh, width, height), x, y, ww, wh, dw, dh);
    }
    void button(const SDL_MouseButtonEvent& e) {
        if (!focused || !texture || e.button < SDL_BUTTON_LEFT || e.button > SDL_BUTTON_RIGHT) return;
        const bool down = e.type == SDL_MOUSEBUTTONDOWN;
        const auto mask = uint16_t(1u << (e.button - 1));
        const uint16_t next = down ? buttons | mask : buttons & ~mask;
        if (next != buttons && send(inputRecord(down ? Input::buttonDown : Input::buttonUp, e.button, 0,
                                                pointer(e.x, e.y), next, modifiers))) buttons = next;
    }
    void wheel(const SDL_MouseWheelEvent& e) {
        if (!focused || !texture) return;
        int x{}, y{};
        SDL_GetMouseState(&x, &y);
        const auto point = pointer(x, y);
        const float sign = e.direction == SDL_MOUSEWHEEL_FLIPPED ? -1 : 1;
        for (const auto& step : wheels.encode(sign * e.preciseX, sign * e.preciseY, mailbox.smoothWheel()))
            if (!send(inputRecord(step.type, step.axis, step.value, point, buttons, modifiers))) break;
    }
    void localClipboard() {
        if (!clipboardEnabled) return;
        const std::unique_ptr<char, decltype(&SDL_free)> current(SDL_GetClipboardText(), SDL_free);
        if (!current) return;
        if (lastSyncedClipboard && *lastSyncedClipboard == current.get()) pendingClipboard.reset();
        else if (std::string_view(current.get()).size() <= maxClipboard) pendingClipboard = current.get();
        else pendingClipboard.reset();
    }
    void redraw() {
        int dw{}, dh{};
        if (SDL_GetRendererOutputSize(renderer.get(), &dw, &dh) < 0 || dw < 1 || dh < 1) return;
        sdlCheck(SDL_RenderClear(renderer.get()) == 0, "clear renderer");
        if (texture) {
            const auto rect = letterbox(dw, dh, width, height);
            const SDL_Rect dest{rect.x, rect.y, rect.w, rect.h};
            sdlCheck(SDL_RenderCopy(renderer.get(), texture.get(), nullptr, &dest) == 0, "draw video texture");
        }
        SDL_RenderPresent(renderer.get());
    }

public:
    Viewer(SkylatchClient* active, const Mailbox& updates, bool clipboard)
        : client(active), mailbox(updates), clipboardEnabled(clipboard) {
        window.reset(SDL_CreateWindow("Skylatch", SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED, 1280, 720,
                     SDL_WINDOW_RESIZABLE | SDL_WINDOW_ALLOW_HIGHDPI));
        sdlCheck(bool(window), "create window");
        renderer.reset(SDL_CreateRenderer(window.get(), -1, SDL_RENDERER_ACCELERATED));
        if (!renderer) renderer.reset(SDL_CreateRenderer(window.get(), -1, SDL_RENDERER_SOFTWARE));
        sdlCheck(bool(renderer), "create renderer");
        sdlCheck(SDL_SetRenderDrawColor(renderer.get(), 0, 0, 0, 255) == 0, "set letterbox color");
        focused = SDL_GetWindowFlags(window.get()) & SDL_WINDOW_INPUT_FOCUS;
        SDL_StartTextInput();
        redraw();
    }
    ~Viewer() {
        SDL_StopTextInput();
        SDL_SetCursor(SDL_GetDefaultCursor());
        SDL_ShowCursor(SDL_ENABLE);
    }
    void releaseAll() {
        deferred.reset();
        composing = commitFromComposition = grabKey = false;
        held.fill(false);
        buttons = modifiers = 0;
        wheels.reset();
        send(inputRecord(Input::releaseAll));
    }
    bool event(const SDL_Event& e) {
        switch (e.type) {
        case SDL_QUIT: return false;
        case SDL_WINDOWEVENT:
            switch (e.window.event) {
            case SDL_WINDOWEVENT_CLOSE: return false;
            case SDL_WINDOWEVENT_FOCUS_LOST: focused = false; releaseAll(); break;
            case SDL_WINDOWEVENT_FOCUS_GAINED: focused = true; break;
            case SDL_WINDOWEVENT_SIZE_CHANGED: case SDL_WINDOWEVENT_EXPOSED: redraw(); break;
            default: break;
            }
            break;
        case SDL_RENDER_DEVICE_RESET: texture.reset(); redraw(); break;
        case SDL_KEYDOWN: case SDL_KEYUP: keyboard(e.key); break;
        case SDL_TEXTEDITING: if (focused) editing(e.edit.text[0] != '\0'); break;
#if SDL_VERSION_ATLEAST(2, 0, 22)
        case SDL_TEXTEDITING_EXT:
            if (focused) editing(e.editExt.text && e.editExt.text[0] != '\0');
            SDL_free(e.editExt.text);
            break;
#endif
        case SDL_TEXTINPUT: text(e.text.text); break;
        case SDL_MOUSEMOTION:
            if (focused && texture)
                send(inputRecord(Input::move, 0, 0, pointer(e.motion.x, e.motion.y), buttons, modifiers), true);
            break;
        case SDL_MOUSEBUTTONDOWN: case SDL_MOUSEBUTTONUP: button(e.button); break;
        case SDL_MOUSEWHEEL: wheel(e.wheel); break;
        case SDL_CLIPBOARDUPDATE: localClipboard(); break;
        default: break;
        }
        return true;
    }
    void tick() {
        if (deferred && Clock::now() >= deferred->expires) flushKey();
        if (pendingClipboard && skylatch_send_clipboard(client, pendingClipboard->data(), pendingClipboard->size()) == SKYLATCH_OK)
            lastSyncedClipboard = std::exchange(pendingClipboard, std::nullopt);
    }
    void clipboard(const std::string& value) {
        if (!clipboardEnabled || value.find('\0') != value.npos) return;
        if (SDL_SetClipboardText(value.c_str()) == 0) {
            lastSyncedClipboard = value;
            pendingClipboard.reset();
        } else std::cerr << "Could not set the local clipboard: " << SDL_GetError() << '\n';
    }
    void cursor(Cursor& c) {
        if (!c.width && !c.height) {
            SDL_SetCursor(SDL_GetDefaultCursor());
            cursorImage.reset();
            SDL_ShowCursor(SDL_ENABLE);
            return;
        }
        bool visible = false;
        for (size_t i = 3; i < c.rgba.size(); i += 4) if (c.rgba[i]) { visible = true; break; }
        if (!visible) { SDL_ShowCursor(SDL_DISABLE); return; }
        const std::unique_ptr<SDL_Surface, SdlFree<SDL_Surface, SDL_FreeSurface>> surface(
            SDL_CreateRGBSurfaceFrom(c.rgba.data(), c.width, c.height, 32, 4 * c.width,
#if SDL_BYTEORDER == SDL_BIG_ENDIAN
                                     0xff000000, 0x00ff0000, 0x0000ff00, 0x000000ff));
#else
                                     0x000000ff, 0x0000ff00, 0x00ff0000, 0xff000000));
#endif
        std::unique_ptr<SDL_Cursor, SdlFree<SDL_Cursor, SDL_FreeCursor>> shape(
            surface ? SDL_CreateColorCursor(surface.get(), c.xhot, c.yhot) : nullptr);
        if (shape) {
            SDL_SetCursor(shape.get());
            cursorImage = std::move(shape);
        } else std::cerr << "Could not create remote cursor: " << SDL_GetError() << '\n';
        SDL_ShowCursor(SDL_ENABLE);
    }
    void present(const AVFrame& frame, PixelLayout layout) {
        const Uint32 format = layout == PixelLayout::nv12 ? SDL_PIXELFORMAT_NV12 : SDL_PIXELFORMAT_IYUV;
        if (!texture || width != frame.width || height != frame.height || format != textureFormat) {
            texture.reset(SDL_CreateTexture(renderer.get(), format, SDL_TEXTUREACCESS_STREAMING, frame.width, frame.height));
            sdlCheck(bool(texture), "create video texture");
            width = frame.width;
            height = frame.height;
            textureFormat = format;
        }
        const auto sizes = planeSizes(layout, width, height);
        const auto plane = [&](size_t i) { return uploadPlane(frame.data[i], frame.linesize[i], sizes[i], scratch[i]); };
        const auto y = plane(0), uv = plane(1);
        int rc{};
        SDL_SetYUVConversionMode(frame.color_range == AVCOL_RANGE_JPEG || frame.format == AV_PIX_FMT_YUVJ420P
                                    ? SDL_YUV_CONVERSION_JPEG : frame.colorspace == AVCOL_SPC_BT709
                                    ? SDL_YUV_CONVERSION_BT709 : frame.colorspace == AVCOL_SPC_UNSPECIFIED
                                    ? SDL_YUV_CONVERSION_AUTOMATIC : SDL_YUV_CONVERSION_BT601);
        if (layout == PixelLayout::nv12) rc = SDL_UpdateNVTexture(texture.get(), nullptr, y.data, y.pitch, uv.data, uv.pitch);
        else {
            const auto v = plane(2);
            rc = SDL_UpdateYUVTexture(texture.get(), nullptr, y.data, y.pitch, uv.data, uv.pitch, v.data, v.pitch);
        }
        sdlCheck(rc == 0, "upload decoded frame");
        redraw();
    }
};

uint32_t waitMicros(Clock::time_point start) {
    const auto elapsed = std::chrono::duration_cast<std::chrono::microseconds>(Clock::now() - start).count();
    return static_cast<uint32_t>(std::clamp<int64_t>(elapsed, 0, std::numeric_limits<uint32_t>::max()));
}

class Stream {
    Decoder decoder;
    SkylatchClient* client;
    Viewer* viewer;
    std::deque<FrameTag> pending;
    uint32_t generation{}, sequence{};
    int width{}, height{};
    uint64_t decoded{}, presented{}, decodeUs{}, waitUs{};
    const char* mode = "software";
    Clock::time_point firstFrame{};

    uint32_t ack() {
        const auto tag = pending.front();
        pending.pop_front();
        const auto wait = waitMicros(tag.arrival);
        for (int attempt = 0; ; ++attempt) {
            const auto rc = skylatch_send_ack(client, tag.generation, tag.sequence, wait);
            if (rc == SKYLATCH_OK || rc == SKYLATCH_ERR_CLOSED) return wait;
            if (rc != SKYLATCH_ERR_QUEUE_FULL || attempt == 50) throw std::runtime_error("could not queue frame ACK");
            std::this_thread::sleep_for(1ms);
        }
    }

public:
    Stream(SkylatchClient* active, Viewer* screen) : client(active), viewer(screen) {}
    void frame(const Frame& incoming) {
        if (incoming.tag.generation != generation || incoming.tag.sequence != sequence + 1) {
            while (!pending.empty()) ack();
            decoder.reset();
        }
        generation = incoming.tag.generation;
        sequence = incoming.tag.sequence;
        pending.push_back(incoming.tag);
        auto began = Clock::now();
        const bool ok = decoder.push(incoming.bytes, sequence, incoming.keyframe,
            [&](const AVFrame& image, int64_t timestamp, const char* decodeMode) {
                if (timestamp == AV_NOPTS_VALUE && !pending.empty()) timestamp = pending.front().sequence;
                if (pending.empty() || timestamp < pending.front().sequence || timestamp > pending.back().sequence) return;
                while (pending.front().sequence < timestamp) ack();
                const auto layout = videoLayout(image);
                decodeUs += waitMicros(began);
                if (viewer) viewer->present(image, layout);
                waitUs += ack();
                ++decoded;
                if (viewer) ++presented;
                width = image.width;
                height = image.height;
                mode = decodeMode;
                if (decoded == 1) firstFrame = Clock::now();
                began = Clock::now();
            });
        if (!ok || pending.size() >= 16) {
            while (!pending.empty()) ack();
            if (ok) decoder.reset();
        }
    }
    bool started() const { return decoded != 0; }
    bool elapsed(unsigned seconds) const { return started() && Clock::now() - firstFrame >= std::chrono::seconds(seconds); }
    void print() const {
        std::printf("resolution=%dx%d frames_decoded=%llu frames_presented=%llu avg_decode_ms=%.3f avg_wait_us=%.1f decode_mode=%s\n",
                    width, height, static_cast<unsigned long long>(decoded), static_cast<unsigned long long>(presented),
                    double(decodeUs) / decoded / 1000.0, double(waitUs) / decoded, mode);
    }
};

int run(Config config) {
    if (config.help) { std::cout << usage; return 0; }
    if (!selectDevice(config)) return 0;
    const bool smoke = config.smokeSeconds != 0;
    // Set up the blocking connection before initializing SDL. Callbacks only enqueue updates.
    Mailbox mailbox(!smoke);
    auto client = connect(config, mailbox);
    std::optional<Viewer> viewer;
    if (!smoke) viewer.emplace(client.get(), mailbox, !config.noClipboard);
    Stream stream(client.get(), viewer ? &*viewer : nullptr);
    const auto connected = Clock::now();
    for (;;) {
        if (viewer) {
            SDL_Event event;
            for (int i = 0; i < 64 && SDL_PollEvent(&event); ++i) {
                if (!viewer->event(event)) { viewer->releaseAll(); return 0; }
            }
            viewer->tick();
        } else {
            if (stream.elapsed(config.smokeSeconds)) { stream.print(); return 0; }
            if (!stream.started() && Clock::now() - connected >= 8s)
                throw std::runtime_error("no H.264 frame decoded within 8 seconds");
        }
        auto update = mailbox.next(smoke ? 20ms : 4ms);
        if (!update) continue;
        if (auto* frame = std::get_if<Frame>(&*update)) stream.frame(*frame);
        else if (auto* cursor = std::get_if<Cursor>(&*update); cursor && viewer) viewer->cursor(*cursor);
        else if (auto* text = std::get_if<std::string>(&*update); text && viewer) viewer->clipboard(*text);
        else if (auto* end = std::get_if<End>(&*update)) {
            if (!end->reason.empty()) throw std::runtime_error(end->reason);
            if (smoke) throw std::runtime_error("stream closed before smoke duration elapsed");
            return 0;
        }
    }
}

} // namespace
} // namespace skylatch

int main(int argc, char** argv) {
    const std::vector<std::string_view> args(argv + 1, argv + argc);
    skylatch::Config config;
    try { config = skylatch::parseCli(args); }
    catch (const std::exception& error) { std::cerr << "skylatch-client: " << error.what() << '\n'; return 2; }
    try { return skylatch::run(std::move(config)); }
    catch (const std::exception& error) { std::cerr << "skylatch-client: " << error.what() << '\n'; return 1; }
}
