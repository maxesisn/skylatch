#pragma once

#include "core/clipboard.hpp"
#include <condition_variable>
#include <cstddef>
#include <deque>
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <vector>

struct lws;

namespace relay {
/**
 * Runs clipboard requests in arrival order on one thread, so a slow clipboard owner never blocks the service thread.
 * A request is queued, then running, then finished with a JSON reply; forget() drops it at any of those steps.
 */
class ClipboardWorker {
 public:
  /// Who asked: an HTTP /clipboard client, or the engine itself for SL1 ClipboardData in either direction.
  enum class Origin { http, apply, observe };
  struct Reply {
    lws* client = nullptr;
    unsigned status = 0;
    std::string body;
    Origin origin = Origin::http;
    /// For apply and observe: whether the backend call worked, and for observe the text it read.
    bool ok = false;
    std::optional<std::string> text;
  };
  static constexpr size_t max_requests = 8;
  ~ClipboardWorker() { stop(); }
  /// Starts the worker; a null backend leaves the clipboard unavailable. wake runs on the worker after each reply.
  void start(std::unique_ptr<ClipboardBackend> backend, std::function<void()> wake);
  bool available() const { return bool(backend_); }
  /// Queues a read, or a write of text; false when the clipboard is unavailable or max_requests are waiting.
  bool submit(lws* client, std::optional<std::string> text);
  /// Queues a write of text a viewer sent over SL1; its reply has Origin::apply.
  bool submitApply(std::string text);
  /// Queues a read for a desktop clipboard change, unless one is already waiting; its reply has Origin::observe.
  bool submitObserve();
  std::vector<Reply> take();
  void forget(lws* client);
  void stop();

 private:
  struct Request {
    lws* client = nullptr;
    std::optional<std::string> text;
    Origin origin = Origin::http;
  };
  std::unique_ptr<ClipboardBackend> backend_;
  std::function<void()> wake_;
  std::mutex mutex_;
  std::condition_variable ready_;
  std::deque<Request> queued_;
  std::vector<Reply> finished_;
  Request running_;
  bool running_active_ = false, dropped_ = false;
  bool stopping_ = false;
  std::thread worker_;
  void run();
};
/// The JSON reply for a clipboard request, given what the backend returned.
ClipboardWorker::Reply clipboardReply(const std::optional<std::string>& text, bool read, bool ok);
/** Remembers text a viewer wrote so the desktop's change notice for that write is not sent back to it. */
class EchoGuard {
 public:
  /// Records the text just written for a viewer.
  void applied(std::string text) { last_ = std::move(text); }
  /// Forgets it, for a write that failed.
  void reset() { last_.reset(); }
  /// False when text is what a viewer wrote last; any other text ends the suppression.
  bool shouldSend(const std::string& text) {
    if (last_ && *last_ == text) return false;
    last_.reset();
    return true;
  }

 private:
  std::optional<std::string> last_;
};
}  // namespace relay
