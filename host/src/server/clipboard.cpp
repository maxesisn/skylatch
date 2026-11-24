#include "clipboard.hpp"
#include "wire.hpp"
#include <algorithm>

namespace relay {
ClipboardWorker::Reply clipboardReply(const std::optional<std::string>& text, bool read, bool ok) {
  const auto reply = [](unsigned status, std::string body) {
    ClipboardWorker::Reply value;
    value.status = status; value.body = std::move(body);
    return value;
  };
  if (!ok || (read && !text)) return reply(503, "{\"error\":\"clipboard_unavailable\"}");
  if (!read) return reply(200, "{\"length\":" + std::to_string(text->size()) + "}");
  if (text->size() > max_clipboard_bytes) return reply(413, "{\"error\":\"clipboard_too_large\"}");
  return reply(200, "{\"text\":" + jsonString(*text) + "}");
}
void ClipboardWorker::start(std::unique_ptr<ClipboardBackend> backend, std::function<void()> wake) {
  backend_ = std::move(backend);
  wake_ = std::move(wake);
  if (backend_) worker_ = std::thread([this] { run(); });
}
bool ClipboardWorker::submit(lws* client, std::optional<std::string> text) {
  std::lock_guard<std::mutex> lock(mutex_);
  if (!backend_ || stopping_ || queued_.size() >= max_requests) return false;
  queued_.push_back({client, std::move(text), Origin::http});
  ready_.notify_one();
  return true;
}
bool ClipboardWorker::submitApply(std::string text) {
  std::lock_guard<std::mutex> lock(mutex_);
  if (!backend_ || stopping_ || queued_.size() >= max_requests) return false;
  queued_.push_back({nullptr, std::move(text), Origin::apply});
  ready_.notify_one();
  return true;
}
bool ClipboardWorker::submitObserve() {
  std::lock_guard<std::mutex> lock(mutex_);
  if (!backend_ || stopping_) return false;
  // A read already waiting sees the newest text, so one more would only repeat it.
  for (const auto& request : queued_) if (request.origin == Origin::observe) return true;
  if (queued_.size() >= max_requests) return false;
  queued_.push_back({nullptr, std::nullopt, Origin::observe});
  ready_.notify_one();
  return true;
}
std::vector<ClipboardWorker::Reply> ClipboardWorker::take() {
  std::lock_guard<std::mutex> lock(mutex_);
  return std::move(finished_);
}
void ClipboardWorker::forget(lws* client) {
  if (!client) return;
  std::lock_guard<std::mutex> lock(mutex_);
  const auto mine = [client](const auto& r) { return r.origin == Origin::http && r.client == client; };
  queued_.erase(std::remove_if(queued_.begin(), queued_.end(), mine), queued_.end());
  finished_.erase(std::remove_if(finished_.begin(), finished_.end(), mine), finished_.end());
  if (running_active_ && mine(running_)) dropped_ = true;
}
void ClipboardWorker::stop() {
  { std::lock_guard<std::mutex> lock(mutex_); stopping_ = true; ready_.notify_one(); }
  if (worker_.joinable()) worker_.join();
  backend_.reset();
}
void ClipboardWorker::run() {
  for (;;) {
    { std::unique_lock<std::mutex> lock(mutex_);
      ready_.wait(lock, [this] { return stopping_ || !queued_.empty(); });
      if (stopping_) return;
      running_ = std::move(queued_.front()); queued_.pop_front();
      running_active_ = true; dropped_ = false;
    }
    const bool read = !running_.text;
    std::optional<std::string> text = running_.text;
    const bool ok = read ? bool(text = backend_->read()) : backend_->write(*text);
    Reply reply;
    if (running_.origin == Origin::http) {
      reply = clipboardReply(text, read, ok);
    } else {
      reply.ok = ok;
      if (read && ok) reply.text = std::move(text);
    }
    { std::lock_guard<std::mutex> lock(mutex_);
      running_active_ = false;
      if (dropped_) continue;
      reply.client = running_.client; reply.origin = running_.origin;
      finished_.push_back(std::move(reply));
    }
    wake_();
  }
}
}  // namespace relay
