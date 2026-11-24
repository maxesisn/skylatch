// SPDX-License-Identifier: Apache-2.0
import AppKit

/// Two-way plain text clipboard sync (SL1 Type 19) with self-echo suppression.
///
/// The pasteboard has no change notification, so a 250 ms main-thread timer compares `changeCount`.
final class ClipboardSync {
    private let pasteboard = NSPasteboard.general
    private let send: (String) -> Bool
    private var timer: Timer?
    private var lastSeenChangeCount: Int
    /// Change count produced by our own write of remote text; that change must not be echoed back.
    private var lastAppliedChangeCount: Int?
    private var lastSyncedText: String?

    static let maxBytes = 1024 * 1024

    /// `send` queues text for the host and returns whether it was accepted.
    init(send: @escaping (String) -> Bool) {
        self.send = send
        lastSeenChangeCount = pasteboard.changeCount  // Existing contents are not shared at connect.
    }

    func start() {
        guard timer == nil else { return }
        let timer = Timer(timeInterval: 0.25, repeats: true) { [weak self] _ in self?.poll() }
        timer.tolerance = 0.05
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    func stop() {
        timer?.invalidate()
        timer = nil
    }

    /// Applies text received from the host.
    func apply(_ text: String) {
        let normalized = text.replacingOccurrences(of: "\r\n", with: "\n")
        if normalized == lastSyncedText && pasteboard.string(forType: .string) == normalized { return }
        pasteboard.clearContents()
        guard pasteboard.setString(normalized, forType: .string) else { return }
        lastAppliedChangeCount = pasteboard.changeCount
        lastSeenChangeCount = pasteboard.changeCount
        lastSyncedText = normalized
    }

    private func poll() {
        let count = pasteboard.changeCount
        guard count != lastSeenChangeCount else { return }
        lastSeenChangeCount = count
        if count == lastAppliedChangeCount { return }
        guard let text = pasteboard.string(forType: .string), !text.isEmpty, text != lastSyncedText,
              text.utf8.count <= Self.maxBytes else { return }
        if send(text) { lastSyncedText = text }
    }
}
