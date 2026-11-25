// SPDX-License-Identifier: Apache-2.0
#include "video.hpp"
#include "audio.hpp"
#include "discovery.hpp"
#include <imm.h>
#include <windowsx.h>
#include <fcntl.h>
#include <io.h>
#include <atomic>
#include <charconv>
#include <condition_variable>
#include <memory>
#include <mutex>
#include <numeric>
#include <thread>
#include <utility>

namespace skylatch {
namespace {
constexpr WORD appIconId = 101;

struct Config {
    std::string hub, direct, token, device;
    unsigned smoke{};
    int swapCmdCtrl{-1}; // -1 = automatic (on for macOS hosts reached through a hub), 0 = off, 1 = on
    bool list{}, noDirect{}, noClipboard{}, noAudio{}, help{};
};

// The viewer is a GUI-subsystem program, so it never owns a console window. For command-line output,
// borrow the parent terminal's console when stdout/stderr are not already redirected.
void ensureConsole() {
    static bool attempted = false;
    if (std::exchange(attempted, true)) return;
    const auto detached = [](DWORD id) { return GetFileType(GetStdHandle(id)) == FILE_TYPE_UNKNOWN; };
    const bool out = detached(STD_OUTPUT_HANDLE), err = detached(STD_ERROR_HANDLE);
    if ((!out && !err) || !AttachConsole(ATTACH_PARENT_PROCESS)) return;
    const auto reopen = [](FILE* stream, DWORD id) {
        FILE* file = nullptr;
        if (freopen_s(&file, "CONOUT$", "w", stream) == 0)
            SetStdHandle(id, reinterpret_cast<HANDLE>(_get_osfhandle(_fileno(stream))));
    };
    if (out) reopen(stdout, STD_OUTPUT_HANDLE);
    if (err) reopen(stderr, STD_ERROR_HANDLE);
}

void usage() {
    std::puts("Usage:\n"
              "  skylatch-client --hub <ws:// or wss:// URL> --token <token> --list\n"
              "  skylatch-client --hub <ws:// or wss:// URL> --token <token> [--device <id>]\n"
              "                  [--no-direct] [--no-clipboard] [--no-audio] [--swap-cmd-ctrl | --no-swap-cmd-ctrl]\n"
              "                  [--smoke-seconds N]\n"
              "  skylatch-client --direct <ws:// or wss:// URL> [--token <64-hex>]\n"
              "                  [--no-clipboard] [--no-audio] [--swap-cmd-ctrl | --no-swap-cmd-ctrl] [--smoke-seconds N]\n"
              "  skylatch-client --help\n\n"
              "Without --device, hub mode selects the sole device or opens a picker.\n"
              "--no-direct forces the hub relay. --no-clipboard disables text clipboard sync.\n"
              "--no-audio does not ask the host for its sound.\n"
              "--swap-cmd-ctrl / --no-swap-cmd-ctrl force the Ctrl<->Cmd swap on or off; by default it\n"
              "is on for macOS devices reached through a hub.\n"
              "--smoke-seconds runs hidden without input, clipboard sync or audio;\n"
              "specify --device if the hub has multiple devices.\n"
              "F11: toggle borderless fullscreen (system keys are captured while fullscreen).\n"
              "Double-tap Right Ctrl (or Ctrl+Alt+G): toggle system key capture.\n"
              "Hold Esc for 1.2 s in fullscreen: leave fullscreen.");
}

bool websocketUrl(std::string_view url) {
    if (url.starts_with("ws://")) url.remove_prefix(5);
    else if (url.starts_with("wss://")) url.remove_prefix(6);
    else return false;
    const auto host = url.substr(0, url.find_first_of("/?#"));
    return !host.empty() && host.find('@') == host.npos && url.find_first_of(" \t\r\n") == url.npos;
}

Config parseArgs(int argc, wchar_t** argv) {
    Config c;
    if (argc == 2 && (std::wstring_view(argv[1]) == L"--help" || std::wstring_view(argv[1]) == L"-h")) {
        c.help = true;
        return c;
    }
    for (int i = 1; i < argc; ++i) {
        const std::wstring_view option(argv[i]);
        if (option == L"--swap-cmd-ctrl" || option == L"--no-swap-cmd-ctrl") {
            require(c.swapCmdCtrl < 0, "duplicate or conflicting Cmd/Ctrl swap option; see --help");
            c.swapCmdCtrl = option == L"--swap-cmd-ctrl" ? 1 : 0;
            continue;
        }
        if (option == L"--list" || option == L"--no-direct" || option == L"--no-clipboard" || option == L"--no-audio") {
            auto& flag = option == L"--list" ? c.list : option == L"--no-direct" ? c.noDirect :
                option == L"--no-clipboard" ? c.noClipboard : c.noAudio;
            require(!flag, "duplicate option; see --help");
            flag = true;
            continue;
        }
        auto dest = option == L"--hub" ? &c.hub : option == L"--direct" ? &c.direct :
            option == L"--token" ? &c.token : option == L"--device" ? &c.device : nullptr;
        require(dest || option == L"--smoke-seconds", "unknown option; see --help");
        require(i + 1 < argc && !std::wstring_view(argv[i + 1]).starts_with(L"--"), "missing option value; see --help");
        auto value = toUtf8(argv[++i]);
        require(!value.empty(), "empty option value; see --help");
        if (dest) {
            require(dest->empty(), "duplicate option; see --help");
            *dest = std::move(value);
        } else {
            require(!c.smoke, "duplicate --smoke-seconds");
            const auto end = value.data() + value.size();
            const auto parsed = std::from_chars(value.data(), end, c.smoke);
            require(parsed.ec == std::errc{} && parsed.ptr == end && c.smoke >= 1 && c.smoke <= 86400,
                "--smoke-seconds must be 1..86400");
        }
    }
    require(c.hub.empty() != c.direct.empty(), "specify exactly one of --hub or --direct; see --help");
    require(websocketUrl(c.hub.empty() ? c.direct : c.hub), "endpoint must be a ws:// or wss:// URL without user information");
    if (!c.hub.empty()) {
        require(!c.token.empty(), "--hub requires --token");
        require(!c.list || (c.device.empty() && !c.noDirect && !c.noClipboard && !c.noAudio && !c.smoke && c.swapCmdCtrl < 0),
            "--list cannot be used with session options");
    } else {
        require(!c.list && c.device.empty() && !c.noDirect, "--list, --device and --no-direct require --hub");
        require(c.token.empty() || (c.token.size() == 64 && c.token.find_first_not_of("0123456789abcdefABCDEF") == c.token.npos),
            "direct --token must be 64 hexadecimal digits");
    }
    return c;
}

constexpr UINT msgDevices = WM_APP + 1, msgConnected = WM_APP + 2, msgCursor = WM_APP + 3, msgEnded = WM_APP + 4;
constexpr UINT msgClipboard = WM_APP + 5, msgFirstFrame = WM_APP + 6, msgMonitors = WM_APP + 7, msgResumed = WM_APP + 8;
constexpr UINT_PTR statusTimer = 1, hudTimer = 2, escTimer = 3, perfTimer = 4;
constexpr auto doubleTapWindow = std::chrono::milliseconds(400);
constexpr UINT escHoldMs = 1200;
constexpr uint8_t hidEscape = 41, hidLCtrl = 224, hidLGui = 227, hidRCtrl = 228, hidRGui = 231;
constexpr size_t maxClipboardBytes = 1024 * 1024;

struct ClipboardClose { ~ClipboardClose() { CloseClipboard(); } };

struct Stats {
    unsigned width{}, height{};
    uint64_t decoded{}, presented{}, decodeUs{}, waitUs{};
    const char* mode = "unknown";
    Clock::time_point firstPresent{};
};
// Per-frame and per-audio-packet measurements of the last second, for the performance readout.
class PerfWindow {
    struct Sample { Clock::time_point at; double value; };
    std::deque<Sample> received, decoded, presented, audio;
    void prune(Clock::time_point now) {
        for (auto* q : {&received, &decoded, &presented, &audio})
            while (!q->empty() && now - q->front().at >= std::chrono::seconds(1)) q->pop_front();
    }
    static std::optional<double> mean(const std::deque<Sample>& q) {
        if (q.empty()) return std::nullopt;
        double sum = 0;
        for (const auto& s : q) sum += s.value;
        return sum / double(q.size());
    }
public:
    void receivedBytes(size_t bytes, Clock::time_point at) { received.push_back({at, double(bytes)}); prune(at); }
    // waitUs (arrival to present) counts only for frames that reached the screen.
    void decodedFrame(uint64_t decodeUs, uint64_t waitUs, bool shown, Clock::time_point at) {
        decoded.push_back({at, double(decodeUs) / 1000.0});
        if (shown) presented.push_back({at, double(waitUs) / 1000.0});
        prune(at);
    }
    void audioBuffered(double ms, Clock::time_point at) { audio.push_back({at, ms}); prune(at); }
    void reset() { received.clear(); decoded.clear(); presented.clear(); audio.clear(); }
    void fill(PerfReadout& r, Clock::time_point now) {
        prune(now);
        double bytes = 0;
        for (const auto& s : received) bytes += s.value;
        r.fps = double(presented.size());
        r.mbps = bytes * 8 / 1e6;
        r.decodeMs = mean(decoded); r.waitMs = mean(presented); r.audioMs = mean(audio);
    }
};

struct QueuedFrame { FrameInfo info; bool key; Bytes annexB; };
struct Cursor { unsigned width, height, xhot, yhot; Bytes rgba; };
struct EndStatus { bool ended{}, clean{}; char message[1024]{}; };
struct Monitor { std::string id; std::wstring name; bool active{}; };

class Client {
    Config config;
    HWND wnd{};
    std::atomic<bool> shown{false};
    bool disconnected{}, paintingVideo{};
    std::atomic<bool> stopping{false};
    std::atomic<bool> resumePending{false}; // Set while a reconnect waits for its first presented frame.
    std::atomic<SkylatchClient*> client{nullptr};
    std::atomic<bool> smoothWheel{false}; // The host accepted smooth-wheel (type 9) records for this session.
    std::atomic<std::shared_ptr<Renderer>> activeRenderer{};
    std::shared_ptr<Hud> hud; // Null in smoke mode: no HUD, no Direct2D.
    std::unique_ptr<AudioPlayer> audio; // Null in smoke mode and with --no-audio; outlives every session.
    std::thread worker;
    std::mutex videoLock, statsLock, cursorLock, endLock, clipboardLock, monitorLock;
    std::condition_variable videoReady;
    bool initialWindowReady{}; // Protected by videoLock.
    std::deque<QueuedFrame> videoQ;
    size_t queuedBytes{};
    Stats stats;
    PerfWindow perf; // Protected by statsLock.
    EndStatus endStatus;
    std::optional<Cursor> cursorIncoming;
    std::optional<std::string> clipboardIncoming;
    std::optional<std::vector<Monitor>> monitorsIncoming; // Protected by monitorLock.
    std::vector<Monitor> monitors;
    bool clipboardListening{};
    std::optional<DWORD> lastAppliedClipSeq;
    std::wstring lastSyncedClipboard;
    std::vector<Device> devices;
    std::wstring deviceName;
    Stats prevStats;
    Clock::time_point connectedAt{}, prevTitle{};
    HCURSOR customCursor{};
    bool hiddenCursor{}, composing{}, fullscreen{}, captureKeys{}, windowedCapture{}, systemTab{}, localG{};
    bool audioOn{true}, swapCmdCtrl{}, userDisconnected{}, rctrlArmed{}, mouseTracking{}, overHud{}, overPerf{}, localO{};
    Clock::time_point rctrlAt{};
    HudAction hudPress{HudAction::None};
    unsigned hudSwallow{}; // Mouse buttons (1 left, 2 right, 4 middle) pressed on the HUD; their releases stay local.
    HHOOK hook{};
    WINDOWPLACEMENT placement{sizeof placement};
    DWORD windowStyle{};
    wchar_t highSurrogate{};
    LONG lastMouseX{}, lastMouseY{};
    uint16_t buttons{}, modifiers{}, lastX{}, lastY{};
    int wheelRemainder[2]{}, smoothRemainder[2]{};
    std::array<bool, 232> held{};
    static inline Client* hookOwner = nullptr;

    Stats snapshot() { std::lock_guard lock(statsLock); return stats; }
    EndStatus status() { std::lock_guard lock(endLock); return endStatus; }
    void report(const char* reason, bool clean = false) noexcept {
        if (stopping) return;
        {
            std::lock_guard lock(endLock);
            if (endStatus.ended) return;
            endStatus.ended = true;
            endStatus.clean = clean;
            std::snprintf(endStatus.message, sizeof endStatus.message, "%s", reason);
        }
        PostMessageW(wnd, msgEnded, 0, 0);
    }
    void clearInputState() {
        held.fill(false); buttons = modifiers = 0;
        wheelRemainder[0] = wheelRemainder[1] = smoothRemainder[0] = smoothRemainder[1] = 0;
        if (GetCapture() == wnd) ReleaseCapture();
    }
    void sendInput(uint8_t type, uint16_t code = 0, uint32_t value = 0) {
        const auto active = client.load();
        if (!active || stopping || config.smoke) return;
        const bool pointer = (type >= 3 && type <= 6) || type == 9, bare = type == 7 || type == 8;
        const auto b = inputRecord(type, code, value, pointer ? lastX : 0, pointer ? lastY : 0,
            bare ? 0 : buttons, bare ? 0 : modifiers);
        const auto rc = skylatch_send_input(active, b.data());
        if (rc == SKYLATCH_OK || rc == SKYLATCH_ERR_NOT_READY || rc == SKYLATCH_ERR_CLOSED ||
            (type == 3 && rc == SKYLATCH_ERR_QUEUE_FULL) || (type == 9 && rc == SKYLATCH_ERR_UNSUPPORTED)) return;
        report("could not queue remote input");
    }
    void releaseAll() { clearInputState(); sendInput(7); }

    static void onFrame(void* ctx, uint32_t generation, uint32_t sequence, uint16_t width, uint16_t height,
        uint64_t, bool key, const uint8_t* data, size_t len) noexcept {
        const auto arrival = Clock::now();
        auto& self = *static_cast<Client*>(ctx);
        if (self.stopping) return;
        try {
            constexpr size_t maxQueuedBytes = 32 * 1024 * 1024;
            require(data && len && len <= maxQueuedBytes && width && height &&
                width <= D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION && height <= D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION,
                "invalid H.264 frame dimensions or length");
            {
                std::lock_guard lock(self.statsLock);
                self.perf.receivedBytes(len, arrival);
            }
            {
                std::lock_guard lock(self.videoLock);
                require(self.videoQ.size() < 256 && len <= maxQueuedBytes - self.queuedBytes, "video decoder queue stalled");
                self.videoQ.push_back({{generation, sequence, width, height, arrival}, key, Bytes(data, data + len)});
                self.queuedBytes += len;
            }
            self.videoReady.notify_one();
        } catch (const std::exception& e) { self.report(e.what()); }
    }
    static void onCursor(void* ctx, uint16_t w, uint16_t h, uint16_t x, uint16_t y,
        const uint8_t* rgba, size_t len) noexcept {
        auto& self = *static_cast<Client*>(ctx);
        if (self.stopping || self.config.smoke) return;
        try {
            {
                std::lock_guard lock(self.cursorLock);
                const bool pending = self.cursorIncoming.has_value();
                self.cursorIncoming = Cursor{w, h, x, y, rgba ? Bytes(rgba, rgba + len) : Bytes{}};
                if (pending) return;
            }
            PostMessageW(self.wnd, msgCursor, 0, 0);
        } catch (const std::exception& e) { self.report(e.what()); }
    }
    static void onInputResponse(void* ctx, uint32_t sequence, uint8_t status) noexcept {
        if (!sequence) static_cast<Client*>(ctx)->smoothWheel = status == SKYLATCH_INPUT_ACCEPTED;
    }
    static void onAudioPcm(void* ctx, uint64_t, uint32_t sampleRate, uint8_t channels, const int16_t* pcm,
        size_t frames) noexcept {
        auto& self = *static_cast<Client*>(ctx);
        if (!self.audio || !self.audio->push(pcm, frames, sampleRate, channels)) return;
        const double ms = self.audio->bufferedMs();
        std::lock_guard lock(self.statsLock);
        self.perf.audioBuffered(ms, Clock::now());
    }
    static void onDisconnected(void* ctx, const char* error) noexcept {
        static_cast<Client*>(ctx)->report(error ? error : "Remote session closed", error == nullptr);
    }
    static void onClipboardData(void* ctx, const char* text, size_t len) noexcept {
        auto& self = *static_cast<Client*>(ctx);
        if (self.stopping || len > maxClipboardBytes || (!text && len)) return;
        try {
            std::lock_guard lock(self.clipboardLock);
            const bool pending = self.clipboardIncoming.has_value();
            self.clipboardIncoming.emplace(text ? text : "", len);
            if (!pending && !PostMessageW(self.wnd, msgClipboard, 0, 0)) self.clipboardIncoming.reset();
        } catch (const std::exception&) {
            // Clipboard synchronization is best-effort and must not end the session.
        }
    }

    static void onMonitorList(void* ctx, const char* raw, size_t len) noexcept {
        auto& self = *static_cast<Client*>(ctx);
        if (self.stopping || self.config.smoke || !raw) return;
        try {
            const auto parsed = json::parse(std::string_view(raw, len));
            std::vector<Monitor> list;
            for (const auto& value : parsed.asArray()) {
                const auto* active = value.field("active");
                list.push_back({value.req("id").asString(), displayText(fromUtf8(value.req("name").asString())),
                    active && active->asBool()});
            }
            {
                std::lock_guard lock(self.monitorLock);
                self.monitorsIncoming = std::move(list);
            }
            PostMessageW(self.wnd, msgMonitors, 0, 0);
        } catch (const std::exception&) {
            // The monitor button is a convenience; a malformed list must not end the session.
        }
    }

    bool clipboardEnabled() const { return !config.smoke && !config.noClipboard; }
    void applyClipboard() {
        std::optional<std::string> incoming;
        { std::lock_guard lock(clipboardLock); incoming = std::exchange(clipboardIncoming, std::nullopt); }
        if (!incoming || stopping || disconnected || incoming->find('\0') != incoming->npos) return;
        const auto wide = fromUtf8(*incoming);
        std::wstring text;
        text.reserve(wide.size());
        for (size_t i = 0; i < wide.size(); ++i) {
            if (wide[i] == L'\n' && (i == 0 || wide[i - 1] != L'\r')) text.push_back(L'\r');
            text.push_back(wide[i]);
        }
        const auto bytes = (text.size() + 1) * sizeof(wchar_t);
        std::unique_ptr<void, decltype(&GlobalFree)> memory(GlobalAlloc(GMEM_MOVEABLE, bytes), GlobalFree);
        if (!memory) return;
        auto dest = GlobalLock(memory.get());
        if (!dest) return;
        memcpy(dest, text.c_str(), bytes);
        GlobalUnlock(memory.get());
        if (!OpenClipboard(wnd)) return;
        ClipboardClose close;
        if (!EmptyClipboard() || !SetClipboardData(CF_UNICODETEXT, memory.get())) return;
        memory.release(); // Clipboard owns the allocation after SetClipboardData succeeds.
        lastAppliedClipSeq = GetClipboardSequenceNumber();
        lastSyncedClipboard = std::move(text);
    }
    std::wstring readClipboard() {
        if (!OpenClipboard(wnd)) return {};
        ClipboardClose close;
        if (lastAppliedClipSeq == GetClipboardSequenceNumber()) return {};
        const HGLOBAL memory = GetClipboardData(CF_UNICODETEXT);
        if (!memory) return {};
        const auto capacity = std::min(GlobalSize(memory) / sizeof(wchar_t), maxClipboardBytes + 1);
        const auto text = static_cast<const wchar_t*>(GlobalLock(memory));
        if (!text) return {};
        struct Unlock { HGLOBAL memory; ~Unlock() { GlobalUnlock(memory); } } unlock{memory};
        const auto end = std::find(text, text + capacity, L'\0');
        return end == text + capacity ? std::wstring{} : std::wstring(text, end);
    }
    void sendClipboard() {
        const auto active = client.load();
        if (!clipboardListening || !active || stopping || disconnected ||
            !IsClipboardFormatAvailable(CF_UNICODETEXT)) return;
        auto text = readClipboard();
        if (text.empty() || text == lastSyncedClipboard) return;
        std::string utf8;
        try { utf8 = toUtf8(text); }
        catch (const std::runtime_error&) { return; } // Other applications may publish malformed UTF-16.
        if (utf8.size() <= maxClipboardBytes && skylatch_send_clipboard(active, utf8.data(), utf8.size()) == SKYLATCH_OK)
            lastSyncedClipboard = std::move(text);
    }
    void removeClipboardListener() {
        if (std::exchange(clipboardListening, false)) RemoveClipboardFormatListener(wnd);
    }

    void repaint() {
        // After a disconnect the kept renderer still repaints the last frame under the reconnect card.
        if (config.smoke || (stopping && !disconnected) || paintingVideo) return;
        if (const auto render = activeRenderer.load()) {
            // DXGI may re-enter the window procedure on this thread during a repaint.
            paintingVideo = true;
            struct Reset { bool& flag; ~Reset() { flag = false; } } reset{paintingVideo};
            render->repaint();
        }
    }
    void showFirstFrame(unsigned width, unsigned height) {
        if (config.smoke || shown || disconnected || stopping || !width || !height) return;
        MONITORINFO mi{sizeof mi};
        require(GetMonitorInfoW(MonitorFromWindow(wnd, MONITOR_DEFAULTTONEAREST), &mi) != FALSE,
            "could not get the viewer monitor work area");
        const LONG workW = mi.rcWork.right - mi.rcWork.left, workH = mi.rcWork.bottom - mi.rcWork.top;
        RECT frame{};
        const auto style = DWORD(GetWindowLongPtrW(wnd, GWL_STYLE)), exStyle = DWORD(GetWindowLongPtrW(wnd, GWL_EXSTYLE));
        require(AdjustWindowRectExForDpi(&frame, style, FALSE, exStyle, GetDpiForWindow(wnd)) != FALSE,
            "could not compute the viewer window frame");
        const LONG frameW = frame.right - frame.left, frameH = frame.bottom - frame.top;
        const LONG maxW = std::max(1L, workW * 85 / 100 - frameW), maxH = std::max(1L, workH * 85 / 100 - frameH);
        const unsigned divisor = std::gcd(width, height);
        const LONG unitW = LONG(width / divisor), unitH = LONG(height / divisor);
        const LONG scale = std::min(maxW / unitW, maxH / unitH);
        // Use exact aspect-ratio multiples where they fit, otherwise round to the pixel grid.
        const RECT fit = scale ? RECT{0, 0, scale * unitW, scale * unitH} : letterbox(maxW, maxH, width, height);
        const LONG outerW = fit.right - fit.left + frameW, outerH = fit.bottom - fit.top + frameH;
        require(SetWindowPos(wnd, nullptr, mi.rcWork.left + (workW - outerW) / 2, mi.rcWork.top + (workH - outerH) / 2,
            outerW, outerH, SWP_NOACTIVATE | SWP_NOZORDER) != FALSE, "could not size the viewer window");
        repaint();
        shown = true;
        ShowWindow(wnd, SW_SHOW);
        repaint();
        {
            std::lock_guard lock(videoLock);
            initialWindowReady = true;
        }
        videoReady.notify_one();
    }

    void stream() {
        try {
            ComRuntime com;
            MfRuntime mf;
            // The renderer outlives a session so the last frame stays on screen under the reconnect card.
            auto render = activeRenderer.load();
            if (!render) {
                render = std::make_shared<Renderer>(wnd, hud);
                activeRenderer.store(render);
            }
            smoothWheel = false;
            const SkylatchCallbacks callbacks{onFrame, onCursor, nullptr, onDisconnected, onInputResponse,
                audio ? onAudioPcm : nullptr};
            char err[1024]{};
            auto active = config.hub.empty()
                ? skylatch_connect_direct(config.direct.c_str(), config.token.empty() ? nullptr : config.token.c_str(),
                    &callbacks, this, err, sizeof err)
                : skylatch_connect_hub(config.hub.c_str(), config.token.c_str(), config.device.c_str(), !config.noDirect,
                    &callbacks, this, err, sizeof err);
            require(active != nullptr, err[0] ? err : "connection failed");
            // Callbacks can queue video before connect returns. Publish before any input or ACK.
            client.store(active);
            if (stopping) return;
            if (!config.smoke) {
                const SkylatchSl1Callbacks sl1Callbacks{onMonitorList, clipboardEnabled() ? onClipboardData : nullptr};
                require(skylatch_set_sl1_callbacks(active, &sl1Callbacks, this) == SKYLATCH_OK,
                    "could not register SL1 callbacks");
            }
            PostMessageW(wnd, msgConnected, 0, 0);

            auto ack = [&](const FrameInfo& f, uint32_t wait) {
                if (stopping) return;
                const auto rc = skylatch_send_ack(active, f.generation, f.seq, wait);
                require(rc == SKYLATCH_OK || rc == SKYLATCH_ERR_CLOSED, "could not queue video ACK");
            };
            auto drop = [&](const FrameInfo& f) { ack(f, elapsedUs(f.arrival)); };
            std::unique_ptr<Decoder> decode;
            uint32_t currentGeneration = 0;
            unsigned currentWidth = 0, currentHeight = 0;
            bool awaitingInitialFit = !config.smoke && !shown;
            while (!stopping) {
                QueuedFrame q;
                {
                    std::unique_lock lock(videoLock);
                    videoReady.wait(lock, [&] { return stopping || !videoQ.empty(); });
                    if (stopping) break;
                    q = std::move(videoQ.front()); videoQ.pop_front(); queuedBytes -= q.annexB.size();
                }
                if (q.info.generation != currentGeneration || q.info.width != currentWidth || q.info.height != currentHeight) {
                    // A size change can leave undecoded frames in the same ACK window.
                    if (decode && q.info.generation == currentGeneration) decode->dropPending(drop);
                    decode.reset();
                    currentGeneration = q.info.generation;
                    currentWidth = q.info.width;
                    currentHeight = q.info.height;
                }
                bool outputStarted = false;
                auto output = [&](IMFSample* sample, const VideoFormat& fmt, const FrameInfo& f, uint32_t us) {
                    if (stopping) return;
                    outputStarted = true;
                    const auto result = render->draw(sample, fmt, f, awaitingInitialFit);
                    const auto now = Clock::now();
                    const auto wait = elapsedUs(f.arrival, now);
                    ack(f, wait);
                    const char* mode = decode->mode(result.dxgiBacked);
                    {
                        std::lock_guard lock(statsLock);
                        stats.width = f.width; stats.height = f.height; stats.mode = mode;
                        ++stats.decoded; stats.decodeUs += us;
                        perf.decodedFrame(us, wait, result.presented, now);
                        if (result.presented) {
                            if (!stats.presented) stats.firstPresent = now;
                            ++stats.presented; stats.waitUs += wait;
                        }
                    }
                    if (resumePending.exchange(false)) PostMessageW(wnd, msgResumed, 0, 0);
                    if (awaitingInitialFit) {
                        require(PostMessageW(wnd, msgFirstFrame, f.width, f.height) != FALSE, "could not queue viewer window sizing");
                        // Let the UI fit and present the first cached frame before drawing any more frames.
                        // draw() has released renderLock; stop() can cancel this wait through videoReady.
                        std::unique_lock lock(videoLock);
                        videoReady.wait(lock, [&] { return stopping || initialWindowReady; });
                        awaitingInitialFit = false;
                    }
                };
                auto feed = [&] { decode->feed(q.annexB, q.info, q.key, output, drop); };
                if (decode) { feed(); continue; }
                if (!q.key) { drop(q.info); continue; }
                try {
                    decode = std::make_unique<Decoder>(render->device.Get(), true);
                    feed();
                } catch (const std::runtime_error&) {
                    // DXVA can reject oversized surfaces only once the first input type is set.
                    // Don't retry a frame that has already reached the renderer.
                    if (outputStarted || (currentWidth <= 4096 && currentHeight <= 4096)) throw;
                    decode.reset();
                    decode = std::make_unique<Decoder>(render->device.Get(), false);
                    feed();
                }
            }
        } catch (const std::exception& e) { report(e.what()); }
    }
    void enumerate() {
        try {
            devices = discover(config.hub, config.token);
            // Joining this worker on msgDevices synchronizes ownership of the vector.
            if (!stopping) PostMessageW(wnd, msgDevices, 0, 0);
        } catch (const std::exception& e) {
            if (config.device.empty()) { report(e.what()); return; }
            // The device is already known; the listing only supplies its name and OS for the viewer UI.
            devices.clear();
            if (!stopping) PostMessageW(wnd, msgDevices, 0, 0);
        }
    }
    void choose() {
        worker.join();
        const Device* chosen = nullptr;
        if (config.device.empty()) {
            require(!devices.empty(), "No devices are online on this hub");
            size_t index = 0;
            if (devices.size() > 1) {
                require(!config.smoke, "multiple devices are online; specify --device for smoke mode");
                const auto picked = pickDevice(wnd, devices);
                if (!picked) { PostQuitMessage(0); return; }
                index = *picked;
            }
            chosen = &devices.at(index);
            config.device = chosen->id;
        } else {
            const auto found = std::find_if(devices.begin(), devices.end(), [&](const Device& d) { return d.id == config.device; });
            if (found != devices.end()) chosen = &*found;
        }
        deviceName = chosen ? chosen->name : fromUtf8(config.device);
        swapCmdCtrl = config.swapCmdCtrl >= 0 ? config.swapCmdCtrl == 1
            : chosen && _wcsicmp(chosen->os.c_str(), L"macos") == 0;
        if (hud) { hud->setDevice(deviceName); syncHud(); }
        worker = std::thread(&Client::stream, this);
    }

    void applyCursor() {
        SetCursor(overHud ? LoadCursorW(nullptr, overPerf ? IDC_SIZEALL : IDC_ARROW) : hiddenCursor ? nullptr :
            customCursor ? customCursor : LoadCursorW(nullptr, IDC_ARROW));
    }

    // --- HUD glue (UI thread) ---
    void syncHud() {
        if (hud) hud->setFlags(captureKeys, swapCmdCtrl, fullscreen);
    }
    void hudTick(bool repaintNow = false) {
        if (!hud) return;
        const auto next = hud->step(Clock::now());
        if (next.repaint || repaintNow) repaint();
        if (next.nextMs) SetTimer(wnd, hudTimer, next.nextMs, nullptr);
        else KillTimer(wnd, hudTimer);
    }
    void toast(const wchar_t* text) {
        if (!hud) return;
        hud->toast(text, Clock::now());
        hudTick(true);
    }
    void applyMonitors() {
        std::optional<std::vector<Monitor>> incoming;
        { std::lock_guard lock(monitorLock); incoming = std::exchange(monitorsIncoming, std::nullopt); }
        if (!incoming || stopping) return;
        monitors = std::move(*incoming);
        const auto active = std::find_if(monitors.begin(), monitors.end(), [](const Monitor& m) { return m.active; });
        if (hud) { hud->setMonitors(active == monitors.end() ? 0 : unsigned(active - monitors.begin()), monitors.size()); hudTick(true); }
    }
    void cycleMonitor() {
        const auto active = client.load();
        if (!active || disconnected || monitors.size() < 2) return;
        const auto current = std::find_if(monitors.begin(), monitors.end(), [](const Monitor& m) { return m.active; });
        const size_t next = ((current == monitors.end() ? 0 : size_t(current - monitors.begin())) + 1) % monitors.size();
        skylatch_select_monitor(active, monitors[next].id.c_str());
    }
    void toggleSwap() {
        releaseAll();
        swapCmdCtrl = !swapCmdCtrl;
        syncHud();
        toast(swapCmdCtrl ? hudText().swapOn : hudText().swapOff);
    }
    void toggleAudio() {
        const auto active = client.load();
        if (!active || disconnected || !audio || !hud) return;
        const bool next = !audioOn;
        if (skylatch_set_audio_enabled(active, next ? 1 : 0) != SKYLATCH_OK) return;
        audioOn = next;
        if (!next) audio->clear();
        hud->setAudio(next);
        toast(next ? hudText().audioOn : hudText().audioOff);
    }
    // Steps the performance readout through hidden, card and strip; it refreshes twice a second while shown.
    void cyclePerf() {
        if (!hud) return;
        if (hud->cyclePerf() == PerfMode::Hidden) KillTimer(wnd, perfTimer);
        else { refreshPerf(); SetTimer(wnd, perfTimer, 500, nullptr); }
        hudTick(true);
    }
    void refreshPerf() {
        if (!hud || disconnected) return;
        PerfReadout r;
        Stats s;
        {
            std::lock_guard lock(statsLock);
            s = stats;
            perf.fill(r, Clock::now());
        }
        const std::string_view mode = s.mode;
        r.width = s.width; r.height = s.height;
        r.decoder = mode == "HW (DXGI surfaces)" ? L"DXGI" : mode == "HW" ? L"DXVA" : mode == "SW" ? L"SW" : L"";
        // The core does not report which path a hub session settled on, so only a forced path is shown.
        r.path = config.hub.empty() ? PerfPath::Direct : config.noDirect ? PerfPath::Relay : PerfPath::Unknown;
        r.audio = !audio ? PerfAudio::Off : audioOn ? PerfAudio::On : PerfAudio::Muted;
        hud->setPerf(r);
    }
    void userDisconnect() {
        if (disconnected || config.smoke) return;
        userDisconnected = true;
        releaseAll();
        report("Disconnected by user", true);
    }
    void reconnect() {
        if (!disconnected || config.smoke || !hud || hud->overlay() != HudOverlay::Lost) return;
        {
            std::lock_guard lock(videoLock);
            videoQ.clear(); queuedBytes = 0; initialWindowReady = true;
            stopping = false;
        }
        { std::lock_guard lock(endLock); endStatus = {}; }
        { std::lock_guard lock(statsLock); stats = {}; prevStats = {}; perf.reset(); }
        { std::lock_guard lock(cursorLock); cursorIncoming.reset(); }
        { std::lock_guard lock(clipboardLock); clipboardIncoming.reset(); }
        userDisconnected = false;
        disconnected = false;
        audioOn = true;
        hud->setAudio(true);
        resumePending = true;
        hud->showReconnecting();
        hudTick(true);
        applyHook();
        worker = std::thread(&Client::stream, this);
    }
    void hudAction(HudAction action) {
        switch (action) {
        case HudAction::Monitor: cycleMonitor(); break;
        case HudAction::Audio: toggleAudio(); break;
        case HudAction::Capture: toggleKeyCapture(); break;
        case HudAction::Swap: toggleSwap(); break;
        case HudAction::Fullscreen: toggleFullscreen(); break;
        case HudAction::Perf: cyclePerf(); break;
        case HudAction::Disconnect: userDisconnect(); break;
        case HudAction::Reconnect: reconnect(); break;
        case HudAction::Close: PostMessageW(wnd, WM_CLOSE, 0, 0); break;
        default: break;
        }
    }
    // Pointer handling: anything the HUD owns is handled locally and never reaches the remote host.
    bool hudMove(LPARAM l) {
        if (!hud) return false;
        const POINT p{GET_X_LPARAM(l), GET_Y_LPARAM(l)};
        if (!mouseTracking) {
            TRACKMOUSEEVENT track{sizeof track, TME_LEAVE, wnd, 0};
            mouseTracking = TrackMouseEvent(&track) != FALSE;
        }
        const bool changed = hud->moved(p, Clock::now());
        const auto action = hud->hit(p);
        const bool over = action != HudAction::None, perfOver = action == HudAction::PerfPanel || hudPress == HudAction::PerfPanel;
        if (over != overHud || perfOver != overPerf) { overHud = over; overPerf = perfOver; applyCursor(); }
        if (changed) hudTick(true);
        return over || hudPress != HudAction::None;
    }
    bool hudButton(UINT m, LPARAM l) {
        if (!hud) return false;
        const POINT p{GET_X_LPARAM(l), GET_Y_LPARAM(l)};
        const bool down = m == WM_LBUTTONDOWN || m == WM_RBUTTONDOWN || m == WM_MBUTTONDOWN;
        const unsigned bit = (m == WM_LBUTTONDOWN || m == WM_LBUTTONUP) ? 1 : (m == WM_RBUTTONDOWN || m == WM_RBUTTONUP) ? 2 : 4;
        if (down) {
            const auto action = hud->hit(p);
            if (action == HudAction::None) return false;
            hudSwallow |= bit;
            if (bit == 1) {
                hudPress = action;
                hud->setPressed(action);
                if (action == HudAction::PerfPanel) hud->beginPerfDrag(p);
                SetFocus(wnd); SetCapture(wnd);
                hudTick(true);
            }
            return true;
        }
        if (!(hudSwallow & bit)) return false;
        hudSwallow &= ~bit;
        if (bit == 1) {
            const auto action = std::exchange(hudPress, HudAction::None);
            hud->setPressed(HudAction::None);
            if (GetCapture() == wnd && !buttons) ReleaseCapture();
            if (action != HudAction::None && action != HudAction::Surface && hud->hit(p) == action) hudAction(action);
            hudTick(true);
        }
        return true;
    }
    bool overHudScreen(LPARAM l) {
        if (!hud) return false;
        POINT p{GET_X_LPARAM(l), GET_Y_LPARAM(l)};
        ScreenToClient(wnd, &p);
        return hud->hit(p) != HudAction::None;
    }
    void changeCursor() {
        std::optional<Cursor> cursor;
        { std::lock_guard lock(cursorLock); cursor = std::exchange(cursorIncoming, std::nullopt); }
        if (!cursor || disconnected) return;
        const auto old = customCursor;
        customCursor = makeCursor(cursor->width, cursor->height, cursor->xhot, cursor->yhot, cursor->rgba);
        hiddenCursor = !cursor->rgba.empty() && cursor->rgba.size() % 4 == 0;
        for (size_t i = 3; hiddenCursor && i < cursor->rgba.size(); i += 4) hiddenCursor = cursor->rgba[i] == 0;
        POINT p{}; GetCursorPos(&p);
        if (WindowFromPoint(p) == wnd) {
            ScreenToClient(wnd, &p);
            RECT r{}; GetClientRect(wnd, &r);
            if (PtInRect(&r, p)) applyCursor();
        }
        if (old) {
            if (GetCursor() == old) SetCursor(LoadCursorW(nullptr, IDC_ARROW));
            DestroyCursor(old);
        }
    }
    void position(LPARAM l, bool screen) {
        POINT p{GET_X_LPARAM(l), GET_Y_LPARAM(l)};
        if (screen) ScreenToClient(wnd, &p);
        lastMouseX = p.x; lastMouseY = p.y;
        RECT rc{}; GetClientRect(wnd, &rc);
        const auto s = snapshot(); const auto r = letterbox(rc.right, rc.bottom, s.width, s.height);
        lastX = normalize(p.x, r.left, r.right); lastY = normalize(p.y, r.top, r.bottom);
    }
    void mouseButton(unsigned b, bool down, LPARAM l) {
        if (!snapshot().width || disconnected) return;
        position(l, false);
        if (down) { SetFocus(wnd); SetCapture(wnd); buttons |= uint16_t(1 << (b - 1)); }
        else buttons &= uint16_t(~(1 << (b - 1)));
        sendInput(down ? 4 : 5, uint16_t(b));
        if (!buttons && GetCapture() == wnd) ReleaseCapture();
    }
    void wheel(unsigned axis, WPARAM w, LPARAM l) {
        if (!snapshot().width || disconnected) return;
        position(l, true);
        // Vertical is positive up in Win32 and down on the wire; horizontal is positive right in both.
        const int delta = GET_WHEEL_DELTA_WPARAM(w) * (axis ? 1 : -1);
        if (delta % WHEEL_DELTA && smoothWheel) {
            // Touchpads and high-resolution wheels report fractions of a notch; one notch is 48 pixels,
            // so a wheel unit is 48 * 16 / 120 = 32 / 5 sixteenths of a pixel.
            auto& rest = smoothRemainder[axis];
            rest += delta * 32;
            const int sixteenths = rest / 5;
            rest -= sixteenths * 5;
            if (sixteenths) sendInput(9, uint16_t(axis), uint32_t(sixteenths));
            return;
        }
        auto& rem = wheelRemainder[axis];
        if ((delta < 0 && rem > 0) || (delta > 0 && rem < 0)) rem = 0;
        rem += delta;
        int pulses = rem / WHEEL_DELTA; rem -= pulses * WHEEL_DELTA;
        while (pulses) {
            const auto part = std::clamp(pulses, -10, 10);
            sendInput(6, uint16_t(axis), uint32_t(part * WHEEL_DELTA)); pulses -= part;
        }
    }
    // Ctrl <-> Cmd swap: local Ctrl becomes the remote GUI/Cmd key and the local Windows key becomes remote Ctrl.
    uint8_t mapModifier(uint8_t code) const {
        if (!swapCmdCtrl) return code;
        switch (code) {
        case hidLCtrl: return hidLGui;
        case hidRCtrl: return hidRGui;
        case hidLGui: return hidLCtrl;
        case hidRGui: return hidRCtrl;
        default: return code;
        }
    }
    void remoteKey(uint8_t code, bool down) {
        code = mapModifier(code);
        if (!code || !client || config.smoke || (!down && !held[code])) return;
        held[code] = down;
        if (code >= 224) {
            const uint16_t bit = uint16_t(1 << (code - 224));
            modifiers = down ? uint16_t(modifiers | bit) : uint16_t(modifiers & ~bit);
        }
        sendInput(down ? 1 : 2, code);
    }
    void key(WPARAM vk, LPARAM l, bool down) {
        const bool repeat = (l & (1L << 30)) != 0;
        if (vk == VK_F11) { if (down && !repeat) toggleFullscreen(); return; }
        if (disconnected) {
            if (down && !repeat && vk == VK_RETURN) reconnect();
            return;
        }
        if (vk == 'G' && ((GetKeyState(VK_CONTROL) < 0 && GetKeyState(VK_MENU) < 0) || localG)) {
            if (down && !repeat) { localG = true; toggleKeyCapture(); }
            if (!down) localG = false;
            return;
        }
        // Ctrl+Shift+O cycles the performance readout; the O never reaches the host.
        if (vk == 'O' && ((GetKeyState(VK_CONTROL) < 0 && GetKeyState(VK_SHIFT) < 0 && GetKeyState(VK_MENU) >= 0) || localO)) {
            if (down && !repeat) { localO = true; cyclePerf(); }
            if (!down) localO = false;
            return;
        }
        if (composing || vk == VK_PROCESSKEY) return;
        const auto code = hid(unsigned(vk), unsigned(l >> 16) & 255, (l & (1L << 24)) != 0);
        if (down && !repeat) {
            // Double-tap Right Ctrl toggles system key capture. The first tap is forwarded like any Ctrl tap;
            // the second is consumed here. Any other key press in between cancels the gesture.
            if (code == hidRCtrl) {
                const auto now = Clock::now();
                if (rctrlArmed && now - rctrlAt <= doubleTapWindow) { rctrlArmed = false; toggleKeyCapture(); return; }
                rctrlArmed = true; rctrlAt = now;
            } else rctrlArmed = false;
        }
        if (vk == VK_ESCAPE) {
            // Short presses forward immediately; only a 1.2 s hold in fullscreen leaves fullscreen.
            if (down && !repeat && fullscreen) SetTimer(wnd, escTimer, escHoldMs, nullptr);
            else if (!down) KillTimer(wnd, escTimer);
        }
        if (vk == VK_PAUSE || vk == VK_CANCEL) { if (down && !repeat) { remoteKey(code, true); remoteKey(code, false); } return; }
        // Some keyboards deliver PrintScreen only on key-up.
        if (vk == VK_SNAPSHOT && !down && !held[code]) remoteKey(code, true);
        remoteKey(code, down);
    }
    void unicode(wchar_t cu, bool fromIme) {
        const uint32_t n = cu;
        if (n >= 0xd800 && n <= 0xdbff) { highSurrogate = cu; return; }
        if (n >= 0xdc00 && n <= 0xdfff) {
            if (highSurrogate) sendInput(8, 0, 0x10000 + ((uint32_t(highSurrogate) - 0xd800) << 10) + n - 0xdc00);
        } else if (n >= 0x80 || (fromIme && n >= 0x20 && n < 0x7f)) sendInput(8, 0, n);
        highSurrogate = 0;
    }
    void placeIme() {
        RECT r{}; GetClientRect(wnd, &r);
        if (const HIMC imc = ImmGetContext(wnd)) {
            COMPOSITIONFORM form{};
            form.dwStyle = CFS_POINT;
            form.ptCurrentPos = {std::clamp(lastMouseX, 0L, std::max(0L, r.right - 1)),
                std::clamp(lastMouseY + 16, 0L, std::max(0L, r.bottom - 1))};
            ImmSetCompositionWindow(imc, &form);
            ImmReleaseContext(wnd, imc);
        }
    }
    bool commitIme() {
        const HIMC imc = ImmGetContext(wnd);
        if (!imc) return false;
        struct Release { HWND wnd; HIMC imc; ~Release() { ImmReleaseContext(wnd, imc); } } release{wnd, imc};
        const auto size = ImmGetCompositionStringW(imc, GCS_RESULTSTR, nullptr, 0);
        if (size < 0 || size % sizeof(wchar_t)) return false;
        if (!size) return true;
        std::wstring text(size / sizeof(wchar_t), L'\0');
        if (ImmGetCompositionStringW(imc, GCS_RESULTSTR, text.data(), DWORD(size)) != size) return false;
        highSurrogate = 0;
        for (wchar_t c : text) unicode(c, true);
        return true;
    }
    void toggleFullscreen() {
        const bool entering = !fullscreen;
        if (entering) {
            GetWindowPlacement(wnd, &placement); windowStyle = DWORD(GetWindowLongPtrW(wnd, GWL_STYLE));
            MONITORINFO mi{sizeof mi}; GetMonitorInfoW(MonitorFromWindow(wnd, MONITOR_DEFAULTTONEAREST), &mi);
            SetWindowLongPtrW(wnd, GWL_STYLE, (windowStyle & ~WS_OVERLAPPEDWINDOW) | WS_POPUP);
            SetWindowPos(wnd, HWND_TOP, mi.rcMonitor.left, mi.rcMonitor.top, mi.rcMonitor.right - mi.rcMonitor.left,
                mi.rcMonitor.bottom - mi.rcMonitor.top, SWP_FRAMECHANGED | SWP_NOOWNERZORDER);
        } else {
            KillTimer(wnd, escTimer);
            SetWindowLongPtrW(wnd, GWL_STYLE, windowStyle); SetWindowPlacement(wnd, &placement);
            SetWindowPos(wnd, nullptr, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED);
        }
        fullscreen = entering;
        // Fullscreen captures system keys automatically; leaving restores the windowed preference.
        if (entering) { windowedCapture = captureKeys; setCapture(true); }
        else setCapture(windowedCapture);
        syncHud();
        toast(entering ? hudText().fullscreenOn : hudText().fullscreenOff);
    }
    // Esc held for escHoldMs in fullscreen: leave fullscreen and make sure the remote sees Esc released.
    void escHeld() {
        KillTimer(wnd, escTimer);
        if (!fullscreen || disconnected) return;
        remoteKey(hidEscape, false);
        toggleFullscreen();
    }
    static LRESULT CALLBACK keyboardHook(int n, WPARAM w, LPARAM l) {
        auto self = hookOwner;
        if (n == HC_ACTION && self && GetForegroundWindow() == self->wnd && GetFocus() == self->wnd) {
            const auto& k = *reinterpret_cast<const KBDLLHOOKSTRUCT*>(l);
            const bool down = w == WM_KEYDOWN || w == WM_SYSKEYDOWN;
            const bool tab = k.vkCode == VK_TAB && ((k.flags & LLKHF_ALTDOWN) || self->systemTab);
            const bool escCombo = k.vkCode == VK_ESCAPE && ((k.flags & LLKHF_ALTDOWN) || GetAsyncKeyState(VK_CONTROL) < 0);
            if (k.vkCode == VK_LWIN || k.vkCode == VK_RWIN || tab || escCombo) {
                if (tab) self->systemTab = down;
                if (!self->composing) self->remoteKey(hid(k.vkCode, k.scanCode, (k.flags & LLKHF_EXTENDED) != 0), down);
                return 1;
            }
        }
        return CallNextHookEx(nullptr, n, w, l);
    }
    void applyHook() {
        if (captureKeys && !disconnected && GetFocus() == wnd && !hook) {
            hookOwner = this;
            hook = SetWindowsHookExW(WH_KEYBOARD_LL, keyboardHook, GetModuleHandleW(nullptr), 0);
            if (!hook) {
                hookOwner = nullptr; captureKeys = false;
                ensureConsole(); std::fprintf(stderr, "WH_KEYBOARD_LL failed: %lu\n", GetLastError());
            }
        }
    }
    void removeHook() {
        if (hook) UnhookWindowsHookEx(std::exchange(hook, nullptr));
        hookOwner = nullptr; systemTab = false;
    }
    void setCapture(bool on) {
        if (captureKeys == on) return;
        releaseAll(); captureKeys = on;
        if (captureKeys) applyHook(); else removeHook();
        syncHud();
    }
    void toggleKeyCapture() {
        if (disconnected) return;
        setCapture(!captureKeys);
        if (!fullscreen) windowedCapture = captureKeys;
        toast(captureKeys ? hudText().captureOn : hudText().captureOff);
    }
    void tick() {
        const auto s = snapshot(); const auto now = Clock::now();
        if (config.smoke) {
            if (s.presented && now - s.firstPresent >= std::chrono::seconds(config.smoke)) PostQuitMessage(0);
        } else {
            const auto sec = std::max(0.001, std::chrono::duration<double>(now - prevTitle).count());
            const auto fps = (s.presented - prevStats.presented) / sec;
            wchar_t title[256]{};
            swprintf_s(title, L"Skylatch | %ux%u | %.1f FPS | F11: Fullscreen | Double-tap Right Ctrl: Capture system keys [%ls]",
                s.width, s.height, fps, captureKeys ? L"On" : L"Off");
            SetWindowTextW(wnd, title);
            if (hud && s.width) {
                const auto frames = s.presented - prevStats.presented;
                const double waitMs = frames ? double(s.waitUs - prevStats.waitUs) / (1000.0 * double(frames)) : 0.0;
                wchar_t label[16]{}, stat[48]{};
                if (s.height >= 2160) swprintf_s(label, L"4K"); else swprintf_s(label, L"%up", s.height);
                swprintf_s(stat, L"%ls\u2003%.0f FPS\u2003%.1fms", label, fps, waitMs);
                hud->setStat(stat);
            }
            prevStats = s; prevTitle = now;
        }
        if (!s.decoded && now - connectedAt >= std::chrono::seconds(8)) report("timed out waiting for H.264 video");
        else if (config.smoke && !s.presented && now - connectedAt >= std::chrono::seconds(8))
            report("the hidden swap chain did not present any frames");
    }
    void stop() {
        removeClipboardListener();
        {
            std::lock_guard lock(videoLock);
            if (stopping.exchange(true)) return;
        }
        videoReady.notify_all();
        if (worker.joinable()) {
            // DXGI can send synchronous messages to the window thread during Present/teardown.
            const HANDLE handle = worker.native_handle();
            while (MsgWaitForMultipleObjectsEx(1, &handle, INFINITE, QS_SENDMESSAGE, MWMO_INPUTAVAILABLE) == WAIT_OBJECT_0 + 1) {
                MSG msg{};
                PeekMessageW(&msg, nullptr, 0, 0, PM_NOREMOVE | PM_QS_SENDMESSAGE);
            }
            worker.join();
        }
        // No decode ACK or input call can race this destroy. Keep HWND and callbacks alive until it returns.
        skylatch_destroy(client.exchange(nullptr));
    }
    void ended() {
        const auto end = status();
        if (!end.ended || disconnected) return;
        disconnected = true;
        removeHook(); clearInputState(); KillTimer(wnd, escTimer);
        stop();
        KillTimer(wnd, statusTimer);
        std::wstring reason;
        try { reason = userDisconnected ? std::wstring(hudText().userDisconnected) : displayText(fromUtf8(end.message)); }
        catch (const std::exception&) { reason = L"Remote session ended"; }
        if (!config.smoke && shown) {
            hiddenCursor = false;
            if (customCursor) {
                if (GetCursor() == customCursor) SetCursor(LoadCursorW(nullptr, IDC_ARROW));
                DestroyCursor(std::exchange(customCursor, nullptr));
            }
            SetWindowTextW(wnd, (L"Skylatch | Disconnected: " + reason).c_str());
            if (hud) { hud->showLost(reason); hudTick(true); } // The card replaces any modal dialog once the window is visible.
        } else {
            if (!config.smoke) MessageBoxW(wnd, reason.c_str(), L"Skylatch", MB_OK | MB_ICONERROR);
            PostQuitMessage(1);
        }
    }
    LRESULT handle(UINT m, WPARAM w, LPARAM l) {
        switch (m) {
        case WM_CLOSE: releaseAll(); PostQuitMessage(0); return 0;
        case msgDevices: choose(); return 0;
        case msgConnected:
            if (!disconnected) {
                if (clipboardEnabled()) clipboardListening = AddClipboardFormatListener(wnd) != FALSE;
                connectedAt = prevTitle = Clock::now();
                require(SetTimer(wnd, statusTimer, config.smoke ? 50 : 1000, nullptr) != 0, "could not start session timer");
            }
            return 0;
        case msgFirstFrame: showFirstFrame(unsigned(w), unsigned(l)); return 0;
        case msgEnded: ended(); return 0;
        case msgMonitors: applyMonitors(); return 0;
        case msgResumed:
            if (hud && !disconnected) { hud->clearOverlay(); hudTick(true); }
            return 0;
        case msgCursor: changeCursor(); return 0;
        case msgClipboard: applyClipboard(); return 0;
        case WM_CLIPBOARDUPDATE: sendClipboard(); return 0;
        case WM_DESTROY: removeClipboardListener(); break;
        case WM_TIMER:
            if (w == statusTimer && !disconnected) tick();
            else if (w == hudTimer) hudTick();
            else if (w == escTimer) escHeld();
            else if (w == perfTimer && !disconnected) { refreshPerf(); repaint(); }
            return 0;
        case WM_SETCURSOR: if (LOWORD(l) == HTCLIENT) { applyCursor(); return TRUE; } break;
        case WM_ERASEBKGND: return TRUE;
        case WM_SIZE:
            if (hud && w != SIZE_MINIMIZED) hud->setView(UINT(LOWORD(l)), UINT(HIWORD(l)), GetDpiForWindow(wnd));
            if (shown && w != SIZE_MINIMIZED) {
                if (paintingVideo) InvalidateRect(wnd, nullptr, FALSE);
                else repaint();
            }
            return 0;
        case WM_PAINT: {
            PAINTSTRUCT ps{}; BeginPaint(wnd, &ps); EndPaint(wnd, &ps);
            if (shown) repaint();
            return 0;
        }
        case WM_DPICHANGED: {
            const auto& r = *reinterpret_cast<const RECT*>(l);
            if (!fullscreen) SetWindowPos(wnd, nullptr, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE | SWP_NOZORDER);
            return 0;
        }
        case WM_SETFOCUS: applyHook(); return 0;
        case WM_KILLFOCUS:
            releaseAll(); removeHook(); composing = false; highSurrogate = 0; localG = localO = false;
            KillTimer(wnd, escTimer); rctrlArmed = false;
            return 0;
        case WM_CAPTURECHANGED:
            if (buttons) releaseAll();
            if (hud && hudPress == HudAction::PerfPanel && reinterpret_cast<HWND>(l) != wnd) {
                // Capture taken away mid-drag (e.g. a system dialog): drop the drag.
                hudPress = HudAction::None; hudSwallow &= ~1u;
                hud->setPressed(HudAction::None);
                hudTick(true);
            }
            return 0;
        case WM_MOUSELEAVE:
            mouseTracking = false;
            if (hud && hudPress == HudAction::None) {
                overHud = overPerf = false;
                hud->left(Clock::now());
                hudTick(true);
            }
            return 0;
        case WM_MOUSEMOVE:
            if (hudMove(l)) return 0;
            position(l, false);
            if (composing) placeIme();
            if (snapshot().width) sendInput(3);
            return 0;
        case WM_LBUTTONDOWN: case WM_LBUTTONUP: if (hudButton(m, l)) return 0; mouseButton(1, m == WM_LBUTTONDOWN, l); return 0;
        case WM_MBUTTONDOWN: case WM_MBUTTONUP: if (hudButton(m, l)) return 0; mouseButton(2, m == WM_MBUTTONDOWN, l); return 0;
        case WM_RBUTTONDOWN: case WM_RBUTTONUP: if (hudButton(m, l)) return 0; mouseButton(3, m == WM_RBUTTONDOWN, l); return 0;
        case WM_MOUSEWHEEL: case WM_MOUSEHWHEEL:
            if (overHudScreen(l)) return 0;
            wheel(m == WM_MOUSEHWHEEL, w, l);
            return 0;
        case WM_KEYDOWN: case WM_SYSKEYDOWN: case WM_KEYUP: case WM_SYSKEYUP:
            key(w, l, m == WM_KEYDOWN || m == WM_SYSKEYDOWN); return 0;
        case WM_SYSCHAR: return 0;
        case WM_IME_STARTCOMPOSITION: releaseAll(); composing = true; highSurrogate = 0; placeIme(); break;
        case WM_IME_ENDCOMPOSITION: composing = false; break;
        case WM_IME_COMPOSITION:
            if ((l & GCS_RESULTSTR) && commitIme()) {
                // Do not let DefWindowProc generate duplicate WM_IME_CHAR messages for this commit.
                l &= ~GCS_RESULTSTR;
                if (!l) return 0;
            }
            break;
        case WM_CHAR: case WM_IME_CHAR: unicode(wchar_t(w), m == WM_IME_CHAR); return 0;
        case WM_UNICHAR:
            if (w == UNICODE_NOCHAR) return TRUE;
            if (w >= 0x20 && w <= 0x10ffff && !(w >= 0xd800 && w <= 0xdfff)) sendInput(8, 0, uint32_t(w));
            return 0;
        }
        return DefWindowProcW(wnd, m, w, l);
    }
    static LRESULT CALLBACK wndProc(HWND h, UINT m, WPARAM w, LPARAM l) {
        auto self = reinterpret_cast<Client*>(GetWindowLongPtrW(h, GWLP_USERDATA));
        if (m == WM_NCCREATE) {
            self = static_cast<Client*>(reinterpret_cast<CREATESTRUCTW*>(l)->lpCreateParams);
            self->wnd = h; SetWindowLongPtrW(h, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(self));
        }
        if (!self) return DefWindowProcW(h, m, w, l);
        try { return self->handle(m, w, l); }
        catch (const std::exception& e) { self->report(e.what()); return 0; }
    }
public:
    explicit Client(Config c) : config(std::move(c)) {}
    ~Client() {
        removeHook(); stop();
        activeRenderer.store(nullptr); // Release the swap chain before its window goes away.
        if (wnd) DestroyWindow(wnd);
        if (customCursor) DestroyCursor(customCursor);
    }
    int run() {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        const auto instance = GetModuleHandleW(nullptr);
        const UINT systemDpi = GetDpiForSystem();
        const auto icon = [&](int cx, int cy) {
            return static_cast<HICON>(LoadImageW(instance, MAKEINTRESOURCEW(appIconId), IMAGE_ICON,
                GetSystemMetricsForDpi(cx, systemDpi), GetSystemMetricsForDpi(cy, systemDpi), LR_DEFAULTCOLOR | LR_SHARED));
        };
        WNDCLASSEXW wc{sizeof wc}; wc.lpfnWndProc = wndProc; wc.hInstance = instance;
        wc.hIcon = icon(SM_CXICON, SM_CYICON); wc.hIconSm = icon(SM_CXSMICON, SM_CYSMICON);
        wc.hCursor = LoadCursorW(nullptr, IDC_ARROW); wc.lpszClassName = L"SkylatchNativeView";
        require(RegisterClassExW(&wc) != 0, "RegisterClassExW failed");
        if (!config.smoke && !config.noAudio) audio = std::make_unique<AudioPlayer>();
        if (!config.smoke) {
            hud = std::make_shared<Hud>();
            if (config.hub.empty()) {
                // Direct sessions have no device record; label the HUD with the endpoint's host.
                auto url = std::string_view(config.direct);
                url.remove_prefix(url.starts_with("wss://") ? 6 : 5);
                deviceName = fromUtf8(url.substr(0, url.find_first_of("/?#")));
                swapCmdCtrl = config.swapCmdCtrl == 1;
            }
            hud->setDevice(deviceName);
            hud->setAudioAvailable(audio != nullptr);
            syncHud();
        }
        int winX = CW_USEDEFAULT, winY = CW_USEDEFAULT, winW = 1100, winH = 730;
        POINT cursor{};
        MONITORINFO mi{sizeof mi};
        if (GetCursorPos(&cursor) && GetMonitorInfoW(MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY), &mi)) {
            const int workW = mi.rcWork.right - mi.rcWork.left;
            const int workH = mi.rcWork.bottom - mi.rcWork.top;
            if (workW > 0 && workH > 0) {
                winW = std::max(640, workW * 80 / 100);
                winH = std::max(400, workH * 80 / 100);
                winX = mi.rcWork.left + (workW - winW) / 2;
                winY = mi.rcWork.top + (workH - winH) / 2;
            }
        }
        // Smoke uses a hidden, non-activating HWND with the same decode and Present calls as the viewer.
        require(CreateWindowExW(config.smoke ? WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW : 0, wc.lpszClassName,
            L"Skylatch | Waiting for video", WS_OVERLAPPEDWINDOW, winX, winY,
            winW, winH, nullptr, nullptr, wc.hInstance, this) != nullptr, "CreateWindowExW failed");
        // Hub mode looks the device up first (name and OS drive the HUD label and the Ctrl/Cmd default).
        const bool lookup = !config.hub.empty() && (config.device.empty() || !config.smoke);
        worker = std::thread(lookup ? &Client::enumerate : &Client::stream, this);
        MSG msg{}; BOOL next;
        while ((next = GetMessageW(&msg, nullptr, 0, 0)) > 0) { TranslateMessage(&msg); DispatchMessageW(&msg); }
        if (next < 0) report("GetMessageW failed");
        stop();
        const auto end = status();
        if (end.ended && (!end.clean || config.smoke)) {
            ensureConsole();
            std::fprintf(stderr, "Skylatch: %s\n", end.message);
            return 1;
        }
        if (config.smoke) {
            ensureConsole();
            const auto s = snapshot();
            require(s.presented > 0 && Clock::now() - s.firstPresent >= std::chrono::seconds(config.smoke),
                "smoke run ended before the requested live duration");
            std::printf("resolution=%ux%u frames_decoded=%llu frames_presented=%llu avg_decode_ms=%.3f avg_wait_us=%.1f decode_mode=%s\n",
                s.width, s.height, s.decoded, s.presented, s.decodeUs / (1000.0 * s.decoded), double(s.waitUs) / s.presented, s.mode);
        }
        return next < 0 ? 1 : int(msg.wParam);
    }
};
}
}

int wmain(int argc, wchar_t** argv) {
    using namespace skylatch;
    Config config;
    try { config = parseArgs(argc, argv); }
    catch (const std::exception& e) { ensureConsole(); std::fprintf(stderr, "Skylatch: %s\n", e.what()); return 2; }
    try {
        if (config.help) { ensureConsole(); usage(); return 0; }
        if (config.list) { ensureConsole(); printDevices(discover(config.hub, config.token)); return 0; }
        if (config.smoke) ensureConsole();
        return Client(std::move(config)).run();
    } catch (const std::exception& e) { ensureConsole(); std::fprintf(stderr, "Skylatch: %s\n", e.what()); return 1; }
}
