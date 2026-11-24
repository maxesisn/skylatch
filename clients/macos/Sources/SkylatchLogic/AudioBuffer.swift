// SPDX-License-Identifier: Apache-2.0

/// Playback buffer for interleaved 16-bit PCM. Output starts once `target` frames are queued and
/// restarts that way after an underrun, so network jitter becomes a short delay instead of
/// crackling; beyond `limit` frames the oldest are dropped, so the delay never builds up.
public struct PcmJitterBuffer {
    public let channels: Int
    public let target: Int
    public let limit: Int
    private var ring: [Int16]
    private var head = 0
    private var count = 0
    private var priming = true

    public init(channels: Int, target: Int, limit: Int) {
        precondition(channels > 0 && target > 0 && limit >= target)
        self.channels = channels
        self.target = target
        self.limit = limit
        ring = [Int16](repeating: 0, count: limit * channels)
    }

    /// 48 kHz stereo with a 40 ms target and a 120 ms limit.
    public static func lowLatency() -> PcmJitterBuffer {
        PcmJitterBuffer(channels: 2, target: 1920, limit: 5760)
    }

    public var queuedFrames: Int { count / channels }

    /// Appends interleaved samples; a partial trailing frame is ignored.
    public mutating func write(_ samples: UnsafeBufferPointer<Int16>) {
        var input = Slice(base: samples, bounds: 0..<(samples.count / channels * channels))
        if input.count > ring.count { input = input.suffix(ring.count) }
        let overflow = count + input.count - ring.count
        if overflow > 0 {
            head = (head + overflow) % ring.count
            count -= overflow
        }
        var tail = (head + count) % ring.count
        for sample in input {
            ring[tail] = sample
            tail = tail + 1 == ring.count ? 0 : tail + 1
        }
        count += input.count
    }

    /// Drops everything queued; output restarts after priming again.
    public mutating func clear() {
        head = 0
        count = 0
        priming = true
    }

    /// Fills `out` with interleaved samples, padding with silence while priming or on underrun.
    /// Returns the number of frames that came from the buffer.
    @discardableResult
    public mutating func read(into out: UnsafeMutableBufferPointer<Int16>) -> Int {
        if priming && count >= target * channels { priming = false }
        let take = priming ? 0 : min(count, out.count / channels * channels)
        for i in 0..<take {
            out[i] = ring[head]
            head = head + 1 == ring.count ? 0 : head + 1
        }
        count -= take
        for i in take..<out.count { out[i] = 0 }
        if !priming && count == 0 { priming = true }
        return take / channels
    }
}
