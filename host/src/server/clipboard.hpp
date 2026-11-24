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
  struct Reply {
    lws* client = nullptr;
    unsigned status = 0;
    std::string body;
  };
  static constexpr size_t max_requests = 8;
  ~ClipboardWorker() { stop(); }
  /// Starts the worker; a null backend leaves the clipboard unavailable. wake runs on the worker after each reply.
  void start(std::unique_ptr<ClipboardBackend> backend, std::function<void()> wake);
  bool available() const { return bool(backend_); }
  /// Queues a read, or a write of text; false when the clipboard is unavailable or max_requests are waiting.
  bool submit(lws* client, std::optional<std::string> text);
  std::vector<Reply> take();
  void forget(lws* client);
  void stop();

 private:
  struct Request {
    lws* client = nullptr;
    std::optional<std::string> text;
  };
  std::unique_ptr<ClipboardBackend> backend_;
  std::function<void()> wake_;
  std::mutex mutex_;
  std::condition_variable ready_;
  std::deque<Request> queued_;
  std::vector<Reply> finished_;
  lws* running_ = nullptr;
  bool stopping_ = false;
  std::thread worker_;
  void run();
};
/// The JSON reply for a clipboard request, given what the backend returned.
ClipboardWorker::Reply clipboardReply(const std::optional<std::string>& text, bool read, bool ok);
}  // namespace relay
