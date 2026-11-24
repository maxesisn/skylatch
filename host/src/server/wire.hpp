#pragma once

#include "core/audio-encoder.hpp"
#include "core/capture.hpp"
#include "core/clipboard.hpp"
#include "core/input.hpp"
#include "core/monitor.hpp"
#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

namespace relay {
constexpr size_t max_input_batch = 256;
uint16_t u16(const uint8_t* p);
uint32_t u32(const uint8_t* p);
void put(uint8_t* p, uint64_t value, unsigned count);
InputEvent inputEvent(const uint8_t* p);
/** Validates an /input Content-Length header and returns 0 or the HTTP error status. */
unsigned inputLength(const std::string& text, uint64_t& length);
/** Validates a /clipboard Content-Length header and returns 0 or the HTTP error status; zero bytes clears the clipboard. */
unsigned clipboardLength(const std::string& text, uint64_t& length);
/** True for UTF-8 text without NUL, the only text the clipboard accepts. */
bool validText(const std::string& text);
/** True for well-formed UTF-8, NUL included; the check SL1 applies to its text payloads. */
bool validUtf8(const std::string& text);
/** A JSON string literal for text, with invalid UTF-8 replaced by U+FFFD. */
std::string jsonString(const std::string& text);
/** The WebSocket a request target opens. */
enum class Socket : uint8_t { none, watch, stream, take, control };
/** A request target: the socket it opens, and whether a /stream also wants clipboard notices. */
struct Target {
  Socket socket = Socket::none;
  bool clipboard = false;
};
/** Classifies /stream?view=1, /stream, /stream?take=1, /stream?clipboard=1, /stream?take=1&clipboard=1, and /control; any other target opens Socket::none. */
Target viewerTarget(const std::string& target);
/** What opening a socket does to the sockets already open. */
struct ConnectionPlan {
  bool open = false, closeStream = false, closeControl = false;
};
constexpr size_t max_viewers = 4;
constexpr uint16_t close_taken = 4001;
constexpr uint16_t close_busy = 4003;
/// Plans a connection without changing the current input owner or any active stream.
ConnectionPlan connectionPlan(Socket socket, size_t viewerCount, bool streamOwnsInput, bool controlOpen,
                              size_t grantViewerCount, bool grant, bool inputViewerHasGrant);
/**
 * The builders below fill out with pre zero bytes of caller padding followed by one NSL1 message.
 * frameMessage and cursorMessage return false when the payload must not be sent.
 */
bool frameMessage(std::vector<uint8_t>& out, size_t pre, uint32_t generation, uint32_t lastSequence, const EncodedFrame& frame,
                  uint16_t width, uint16_t height);
bool cursorMessage(std::vector<uint8_t>& out, size_t pre, uint32_t generation, const CursorImage& cursor);
void controlReady(std::vector<uint8_t>& out, size_t pre, uint32_t generation, uint16_t width, uint16_t height);
/** Tells a controlling /stream viewer that the desktop clipboard changed; it reads the text from /clipboard. */
void clipboardNotice(std::vector<uint8_t>& out, size_t pre, uint32_t generation);
/// SL1 limits on the payload bytes of Types 17 to 19, as in docs/PROTOCOL.md.
constexpr size_t max_monitor_list_bytes = 64 * 1024;
constexpr size_t max_monitor_id_bytes = 256;
constexpr size_t max_clipboard_data_bytes = 1024 * 1024;
constexpr size_t nsl1_header_bytes = 20;
/**
 * The JSON array of MonitorInfo objects as Type 17 carries it: members in the order id, name, x, y, width, height,
 * primary, active, without whitespace. Trailing monitors are left out when the array would pass max_monitor_list_bytes.
 */
std::string monitorListJson(const std::vector<MonitorInfo>& monitors);
/** Type 17 MonitorList, host to viewer. */
void monitorListMessage(std::vector<uint8_t>& out, size_t pre, uint32_t generation, const std::vector<MonitorInfo>& monitors);
/** Type 18 SelectMonitor, viewer to host; false when id is empty, longer than max_monitor_id_bytes or not UTF-8. */
bool selectMonitorMessage(std::vector<uint8_t>& out, size_t pre, uint32_t generation, const std::string& id);
/** Type 19 ClipboardData in either direction; false when text is longer than max_clipboard_data_bytes or not UTF-8. */
bool clipboardDataMessage(std::vector<uint8_t>& out, size_t pre, uint32_t generation, const std::string& text);
/** Reads a SelectMonitor payload; false when it is empty, too long or not UTF-8. */
bool selectMonitorId(const uint8_t* payload, size_t size, std::string& id);
/** Reads a ClipboardData payload; false when it is too long or not UTF-8. */
bool clipboardDataText(const uint8_t* payload, size_t size, std::string& text);
/** The largest total size, header included, a received viewer message may reach, judged from the type byte once it is known. */
size_t maxViewerMessage(const std::vector<uint8_t>& received);
void inputReply(std::vector<uint8_t>& out, size_t pre, uint32_t generation, uint32_t sequence, InputStatus status);
/// SL1 audio limits, as in docs/PROTOCOL.md.
constexpr size_t max_audio_config_bytes = 4096;
constexpr size_t max_audio_frame_bytes = 16 * 1024;
constexpr size_t audio_frame_prefix_bytes = 8;
/** Type 20 AudioConfig, host to viewer: the 48 kHz stereo 20 ms Opus format AudioEncoder produces. */
void audioConfigMessage(std::vector<uint8_t>& out, size_t pre, uint32_t generation);
/** Type 21 AudioFrame, host to viewer; false when sequence is zero or the packet is empty or too long. */
bool audioFrameMessage(std::vector<uint8_t>& out, size_t pre, uint32_t generation, uint32_t sequence, const AudioPacket& packet);
/** Reads a Type 22 AudioControl payload; false unless it is the single byte 0 (disable) or 1 (enable). */
bool audioControlEnabled(const uint8_t* payload, size_t size, bool& enabled);
}  // namespace relay
