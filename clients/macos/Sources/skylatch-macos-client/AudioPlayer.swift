// SPDX-License-Identifier: Apache-2.0
import AudioToolbox
import Foundation
import SkylatchLogic

/// Plays the host's 48 kHz stereo PCM through an AudioQueue that pulls 10 ms buffers from a
/// low-latency jitter buffer. Pushing is thread-safe; the queue's own thread drains it.
final class AudioPlayer {
    private static let sampleRate = 48_000
    private static let bufferFrames = 480
    private static let bufferCount = 3

    private var queue: AudioQueueRef?
    private let lock = NSLock()
    private var jitter = PcmJitterBuffer.lowLatency()
    private var stereo: [Int16] = []

    /// Nil when the system has no usable audio output.
    init?() {
        var format = AudioStreamBasicDescription(
            mSampleRate: Float64(Self.sampleRate), mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kLinearPCMFormatFlagIsSignedInteger | kLinearPCMFormatFlagIsPacked,
            mBytesPerPacket: 4, mFramesPerPacket: 1, mBytesPerFrame: 4, mChannelsPerFrame: 2,
            mBitsPerChannel: 16, mReserved: 0)
        let context = Unmanaged.passUnretained(self).toOpaque()
        var created: AudioQueueRef?
        guard AudioQueueNewOutput(&format, { context, queue, buffer in
            Unmanaged<AudioPlayer>.fromOpaque(context!).takeUnretainedValue().fill(queue, buffer)
        }, context, nil, nil, 0, &created) == noErr, let created else { return nil }
        queue = created
        for _ in 0..<Self.bufferCount {
            var buffer: AudioQueueBufferRef?
            guard AudioQueueAllocateBuffer(created, UInt32(Self.bufferFrames * 4), &buffer) == noErr, let buffer else {
                stop()
                return nil
            }
            fill(created, buffer)
        }
        guard AudioQueueStart(created, nil) == noErr else {
            stop()
            return nil
        }
    }

    /// Queues decoded PCM; mono is played on both channels and other sample rates are ignored.
    func push(_ pcm: UnsafePointer<Int16>, frames: Int, channels: Int, sampleRate: UInt32) {
        guard sampleRate == Self.sampleRate, channels == 1 || channels == 2 else { return }
        lock.lock()
        defer { lock.unlock() }
        if channels == 2 {
            jitter.write(UnsafeBufferPointer(start: pcm, count: frames * 2))
        } else {
            stereo.removeAll(keepingCapacity: true)
            for i in 0..<frames { stereo.append(pcm[i]); stereo.append(pcm[i]) }
            stereo.withUnsafeBufferPointer { jitter.write($0) }
        }
    }

    func stop() {
        guard let queue else { return }
        self.queue = nil
        AudioQueueStop(queue, true)
        AudioQueueDispose(queue, true)
    }

    deinit { stop() }

    private func fill(_ queue: AudioQueueRef, _ buffer: AudioQueueBufferRef) {
        let samples = buffer.pointee.mAudioData.bindMemory(to: Int16.self, capacity: Self.bufferFrames * 2)
        lock.lock()
        jitter.read(into: UnsafeMutableBufferPointer(start: samples, count: Self.bufferFrames * 2))
        lock.unlock()
        buffer.pointee.mAudioDataByteSize = UInt32(Self.bufferFrames * 4)
        AudioQueueEnqueueBuffer(queue, buffer, 0, nil)
    }
}
