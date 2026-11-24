#include "clipboard.hpp"
#include "wire.hpp"
#include <algorithm>

namespace relay {
ClipboardWorker::Reply clipboardReply(const std::optional<std::string>& text, bool read, bool ok) {
  if (!ok || (read && !text)) return {nullptr, 503, "{\"error\":\"clipboard_unavailable\"}"};
  if (!read) return {nullptr, 200, "{\"length\":" + std::to_string(text->size()) + "}"};
  if (text->size() > max_clipboard_bytes) return {nullptr, 413, "{\"error\":\"clipboard_too_large\"}"};
  return {nullptr, 200, "{\"text\":" + jsonString(*text) + "}"};
}
void ClipboardWorker::start(std::unique_ptr<ClipboardBackend> backend, std::function<void()> wake) {
  backend_ = std::move(backend);
  wake_ = std::move(wake);
  if (backend_) worker_ = std::thread([this] { run(); });
}
bool ClipboardWorker::submit(lws* client, std::optional<std::string> text) {
  std::lock_guard<std::mutex> lock(mutex_);
  if (!backend_ || stopping_ || queued_.size() >= max_requests) return false;
  queued_.push_back({client, std::move(text)});
  ready_.notify_one();
  return true;
}
std::vector<ClipboardWorker::Reply> ClipboardWorker::take() {
  std::lock_guard<std::mutex> lock(mutex_);
  return std::move(finished_);
}
void ClipboardWorker::forget(lws* client) {
  std::lock_guard<std::mutex> lock(mutex_);
  queued_.erase(std::remove_if(queued_.begin(), queued_.end(), [client](const Request& r) { return r.client == client; }),
                queued_.end());
  finished_.erase(std::remove_if(finished_.begin(), finished_.end(), [client](const Reply& r) { return r.client == client; }),
                  finished_.end());
  if (running_ == client) running_ = nullptr;
}
void ClipboardWorker::stop() {
  { std::lock_guard<std::mutex> lock(mutex_); stopping_ = true; ready_.notify_one(); }
  if (worker_.joinable()) worker_.join();
  backend_.reset();
}
void ClipboardWorker::run() {
  for (;;) {
    Request request;
    { std::unique_lock<std::mutex> lock(mutex_);
      ready_.wait(lock, [this] { return stopping_ || !queued_.empty(); });
      if (stopping_) return;
      request = std::move(queued_.front()); queued_.pop_front();
      running_ = request.client;
    }
    const bool read = !request.text;
    std::optional<std::string> text = request.text;
    const bool ok = read ? bool(text = backend_->read()) : backend_->write(*text);
    auto reply = clipboardReply(text, read, ok);
    { std::lock_guard<std::mutex> lock(mutex_);
      if (!running_) continue;
      reply.client = running_; running_ = nullptr;
      finished_.push_back(std::move(reply));
    }
    wake_();
  }
}
}  // namespace relay
