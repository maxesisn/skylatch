// SPDX-License-Identifier: Apache-2.0
import CSkylatchCore
import CoreVideo
import Foundation
import SkylatchLogic

struct SessionStats {
    var width = 0
    var height = 0
    var decoded: UInt64 = 0
    var presented: UInt64 = 0
    var decodeUs: UInt64 = 0
    var waitUs: UInt64 = 0
    var mode = "unknown"
    var firstPresent: UInt64 = 0  // uptime ns
}

struct SessionEnd {
    var message: String
    var clean: Bool
}

/// Receives session events on the main thread.
protocol SessionDelegate: AnyObject {
    func sessionConnected()
    func sessionEnded(_ end: SessionEnd)
    func sessionCursor(width: Int, height: Int, xhot: Int, yhot: Int, rgba: [UInt8]?)
    func sessionClipboardText(_ text: String)
}

/// One connection to a host: the core client, the decode/present thread, and the shared statistics.
final class Session: @unchecked Sendable {
    private struct QueuedFrame {
        var generation: UInt32
        var sequence: UInt32
        var width: Int
        var height: Int
        var arrival: UInt64
        var keyframe: Bool
        var data: [UInt8]
    }

    private let config: Config
    private let renderer: MetalRenderer
    private weak var delegate: SessionDelegate?
    private let clipboardEnabled: Bool

    private let queueLock = NSCondition()
    private var queue: [QueuedFrame] = []
    private var queuedBytes = 0
    private var stopping = false

    private let stateLock = NSLock()
    private var client: OpaquePointer?
    private var stats = SessionStats()
    private var end: SessionEnd?
    private var thread: Thread?

    private static let maxQueuedBytes = 32 * 1024 * 1024
    private static let maxClipboardBytes = 1024 * 1024

    init(config: Config, renderer: MetalRenderer, delegate: SessionDelegate) {
        self.config = config
        self.renderer = renderer
        self.delegate = delegate
        self.clipboardEnabled = !config.isSmoke && !config.noClipboard
    }

    // MARK: Thread-safe accessors

    func snapshot() -> SessionStats {
        stateLock.lock()
        defer { stateLock.unlock() }
        return stats
    }

    var endStatus: SessionEnd? {
        stateLock.lock()
        defer { stateLock.unlock() }
        return end
    }

    private func isStopping() -> Bool {
        queueLock.lock()
        defer { queueLock.unlock() }
        return stopping
    }

    /// Records the first end condition and wakes the main thread.
    func report(_ message: String, clean: Bool = false) {
        if isStopping() { return }
        stateLock.lock()
        if end != nil { stateLock.unlock(); return }
        end = SessionEnd(message: message, clean: clean)
        let value = end!
        stateLock.unlock()
        DispatchQueue.main.async { [weak self] in self?.delegate?.sessionEnded(value) }
    }

    // MARK: Lifecycle

    /// Connects and decodes on a dedicated thread. `device` is the hub device ID (ignored for direct mode).
    func start(device: String) {
        let thread = Thread { [self] in stream(device: device) }
        thread.name = "skylatch.stream"
        thread.qualityOfService = .userInteractive
        thread.stackSize = 4 << 20
        self.thread = thread
        thread.start()
    }

    /// Stops the worker and destroys the core client. Call from the main thread, once.
    func stop() {
        queueLock.lock()
        if stopping { queueLock.unlock(); return }
        stopping = true
        queueLock.broadcast()
        queueLock.unlock()
        // Wait for the worker so no ACK can race the destroy below.
        while let thread, !thread.isFinished { Thread.sleep(forTimeInterval: 0.002) }
        stateLock.lock()
        let active = client
        client = nil
        stateLock.unlock()
        skylatch_destroy(active)
    }

    // MARK: Sending from the main thread

    func sendInput(_ record: [UInt8]) {
        stateLock.lock()
        let active = client
        stateLock.unlock()
        guard let active, !isStopping(), record.count == 16 else { return }
        let rc = record.withUnsafeBufferPointer { skylatch_send_input(active, $0.baseAddress) }
        if rc == SKYLATCH_OK || rc == SKYLATCH_ERR_NOT_READY || rc == SKYLATCH_ERR_CLOSED { return }
        // A full queue only drops pointer motion; anything else means the connection is unusable.
        if rc == SKYLATCH_ERR_QUEUE_FULL && record[0] == InputType.pointerMove { return }
        report("could not queue remote input")
    }

    func sendClipboard(_ text: String) -> Bool {
        stateLock.lock()
        let active = client
        stateLock.unlock()
        guard let active, !isStopping() else { return false }
        let bytes = Array(text.utf8)
        guard bytes.count <= Self.maxClipboardBytes else { return false }
        return bytes.withUnsafeBufferPointer {
            skylatch_send_clipboard(active, UnsafeRawPointer($0.baseAddress)?.assumingMemoryBound(to: CChar.self), bytes.count)
        } == SKYLATCH_OK
    }

    // MARK: Callbacks (connection worker thread)

    private static func session(_ ctx: UnsafeMutableRawPointer?) -> Session {
        Unmanaged<Session>.fromOpaque(ctx!).takeUnretainedValue()
    }

    private func enqueue(generation: UInt32, sequence: UInt32, width: Int, height: Int, keyframe: Bool,
                         data: UnsafePointer<UInt8>?, length: Int) {
        let arrival = DispatchTime.now().uptimeNanoseconds
        guard !isStopping() else { return }
        guard let data, length > 0, length <= Self.maxQueuedBytes, width > 0, height > 0, width <= 16384, height <= 16384 else {
            report("invalid H.264 frame dimensions or length")
            return
        }
        let bytes = Array(UnsafeBufferPointer(start: data, count: length))
        queueLock.lock()
        if queue.count >= 256 || length > Self.maxQueuedBytes - queuedBytes {
            queueLock.unlock()
            report("video decoder queue stalled")
            return
        }
        queue.append(QueuedFrame(generation: generation, sequence: sequence, width: width, height: height,
                                 arrival: arrival, keyframe: keyframe, data: bytes))
        queuedBytes += length
        queueLock.signal()
        queueLock.unlock()
    }

    private func dequeue() -> QueuedFrame? {
        queueLock.lock()
        defer { queueLock.unlock() }
        while queue.isEmpty && !stopping { queueLock.wait() }
        if stopping { return nil }
        let frame = queue.removeFirst()
        queuedBytes -= frame.data.count
        return frame
    }

    // MARK: Worker

    private func stream(device: String) {
        var callbacks = SkylatchCallbacks()
        callbacks.on_frame = { ctx, generation, sequence, width, height, _, key, data, length in
            Session.session(ctx).enqueue(generation: generation, sequence: sequence, width: Int(width), height: Int(height),
                                         keyframe: key, data: data, length: length)
        }
        callbacks.on_cursor = { ctx, width, height, xhot, yhot, rgba, length in
            let me = Session.session(ctx)
            if me.isStopping() || me.config.isSmoke { return }
            let pixels = rgba.map { Array(UnsafeBufferPointer(start: $0, count: length)) }
            DispatchQueue.main.async {
                me.delegate?.sessionCursor(width: Int(width), height: Int(height), xhot: Int(xhot), yhot: Int(yhot), rgba: pixels)
            }
        }
        callbacks.on_disconnected = { ctx, error in
            let me = Session.session(ctx)
            me.report(error.map { String(cString: $0) } ?? "Remote session closed", clean: error == nil)
        }

        var err = [CChar](repeating: 0, count: 1024)
        let context = Unmanaged.passUnretained(self).toOpaque()
        let active: OpaquePointer? = config.hub.isEmpty
            ? skylatch_connect_direct(config.direct, config.token.isEmpty ? nil : config.token, &callbacks, context, &err, err.count)
            : skylatch_connect_hub(config.hub, config.token, device, !config.noDirect, &callbacks, context, &err, err.count)
        guard let active else {
            let text = String(cString: err)
            report(text.isEmpty ? "connection failed" : text)
            return
        }
        // Frames can arrive before connect returns; publish the handle before any ACK or input.
        stateLock.lock()
        client = active
        stateLock.unlock()
        if isStopping() { return }

        if clipboardEnabled {
            var sl1 = SkylatchSl1Callbacks()
            sl1.on_clipboard_data = { ctx, text, length in
                let me = Session.session(ctx)
                guard !me.isStopping(), length <= Session.maxClipboardBytes, text != nil || length == 0 else { return }
                let bytes = text.map { Array(UnsafeRawBufferPointer(start: $0, count: length)) } ?? []
                guard let string = String(validatingUTF8Bytes: bytes), !string.contains("\0") else { return }
                DispatchQueue.main.async { me.delegate?.sessionClipboardText(string) }
            }
            guard skylatch_set_sl1_callbacks(active, &sl1, context) == SKYLATCH_OK else {
                report("could not register clipboard callback")
                return
            }
        }
        DispatchQueue.main.async { [weak self] in self?.delegate?.sessionConnected() }
        decodeLoop(active)
    }

    private func decodeLoop(_ active: OpaquePointer) {
        let decoder = H264Decoder()
        var currentGeneration: UInt32 = 0
        var currentWidth = 0, currentHeight = 0
        var failures = 0

        func ack(_ frame: QueuedFrame, wait: UInt64) {
            if isStopping() { return }
            let capped = UInt32(min(wait, UInt64(UInt32.max)))
            let rc = skylatch_send_ack(active, frame.generation, frame.sequence, capped)
            if rc != SKYLATCH_OK && rc != SKYLATCH_ERR_CLOSED { report("could not queue video ACK") }
        }
        func elapsedUs(since start: UInt64) -> UInt64 {
            (DispatchTime.now().uptimeNanoseconds &- start) / 1000
        }
        func drop(_ frame: QueuedFrame) { ack(frame, wait: elapsedUs(since: frame.arrival)) }

        while let frame = dequeue() {
            if frame.generation != currentGeneration || frame.width != currentWidth || frame.height != currentHeight {
                // A new stream or a resized desktop: start from a clean decoder at the next keyframe.
                decoder.invalidate()
                currentGeneration = frame.generation
                currentWidth = frame.width
                currentHeight = frame.height
            }
            if !decoder.isReady && !frame.keyframe {
                drop(frame)
                continue
            }
            let unit = AnnexB.parse(frame.data)
            do {
                guard let result = try decoder.decode(unit) else {
                    drop(frame)
                    continue
                }
                failures = 0
                let presented = renderer.draw(result.buffer)
                let now = DispatchTime.now().uptimeNanoseconds
                let wait = (now &- frame.arrival) / 1000
                ack(frame, wait: wait)
                stateLock.lock()
                stats.width = frame.width
                stats.height = frame.height
                stats.mode = decoder.usesHardware ? "VideoToolbox" : "VideoToolbox-software"
                stats.decoded += 1
                stats.decodeUs += result.decodeUs
                if presented {
                    if stats.presented == 0 { stats.firstPresent = now }
                    stats.presented += 1
                    stats.waitUs += wait
                }
                stateLock.unlock()
            } catch {
                // Drop the session and wait for the next keyframe; give up if keyframes keep failing.
                decoder.invalidate()
                drop(frame)
                failures += frame.keyframe ? 1 : 0
                if failures >= 3 { report("\(error)") }
            }
        }
    }
}

extension String {
    /// Strict UTF-8 decoding: nil for malformed input.
    init?(validatingUTF8Bytes bytes: [UInt8]) {
        guard let s = String(bytes: bytes, encoding: .utf8) else { return nil }
        self = s
    }
}
