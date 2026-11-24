// SPDX-License-Identifier: Apache-2.0
import CoreMedia
import CoreVideo
import Foundation
import SkylatchLogic
import VideoToolbox

struct DecodeError: Error, CustomStringConvertible {
    let message: String
    init(_ message: String) { self.message = message }
    init(_ what: String, status: OSStatus) { self.message = "\(what) failed (OSStatus \(status))" }
    var description: String { message }
}

/// Collects the output of one asynchronous VideoToolbox decode call.
private final class DecodeResult: @unchecked Sendable {
    private let lock = NSLock()
    private var buffer: CVPixelBuffer?
    private var status: OSStatus = noErr
    private var finishedAt: UInt64 = 0

    func complete(status: OSStatus, buffer: CVPixelBuffer?) {
        lock.lock()
        self.status = status
        self.buffer = buffer
        finishedAt = DispatchTime.now().uptimeNanoseconds
        lock.unlock()
    }

    func take() -> (OSStatus, CVPixelBuffer?, UInt64) {
        lock.lock()
        defer { lock.unlock() }
        return (status, buffer, finishedAt)
    }
}

/// H.264 decoder on top of a VideoToolbox session that is rebuilt whenever the SPS or PPS changes.
final class H264Decoder {
    private var session: VTDecompressionSession?
    private var format: CMVideoFormatDescription?
    private var sps: [UInt8] = []
    private var pps: [UInt8] = []
    private var counter: Int64 = 0

    /// Whether the active session runs on a hardware decoder (false until the first session exists).
    private(set) var usesHardware = false

    var isReady: Bool { session != nil }

    deinit { invalidate() }

    /// Drops the session; the next keyframe with parameter sets creates a new one.
    func invalidate() {
        if let session {
            VTDecompressionSessionWaitForAsynchronousFrames(session)
            VTDecompressionSessionInvalidate(session)
        }
        session = nil
        format = nil
        sps = []
        pps = []
    }

    private func createSession(sps newSPS: [UInt8], pps newPPS: [UInt8]) throws {
        invalidate()
        var description: CMVideoFormatDescription?
        let formatStatus = newSPS.withUnsafeBufferPointer { spsBuffer in
            newPPS.withUnsafeBufferPointer { ppsBuffer -> OSStatus in
                let pointers: [UnsafePointer<UInt8>] = [spsBuffer.baseAddress!, ppsBuffer.baseAddress!]
                let sizes = [spsBuffer.count, ppsBuffer.count]
                return CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: 2, parameterSetPointers: pointers,
                    parameterSetSizes: sizes, nalUnitHeaderLength: 4, formatDescriptionOut: &description)
            }
        }
        guard formatStatus == noErr, let description else {
            throw DecodeError("CMVideoFormatDescriptionCreateFromH264ParameterSets", status: formatStatus)
        }

        let specification: [CFString: Any] = [
            kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: true,
        ]
        let attributes: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            kCVPixelBufferMetalCompatibilityKey: true,
            kCVPixelBufferIOSurfacePropertiesKey: [String: Any]() as CFDictionary,
        ]
        var created: VTDecompressionSession?
        let status = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault, formatDescription: description,
            decoderSpecification: specification as CFDictionary, imageBufferAttributes: attributes as CFDictionary,
            outputCallback: nil, decompressionSessionOut: &created)
        guard status == noErr, let created else { throw DecodeError("VTDecompressionSessionCreate", status: status) }
        VTSessionSetProperty(created, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)

        var hardware: Unmanaged<CFBoolean>?
        let queried = withUnsafeMutablePointer(to: &hardware) {
            VTSessionCopyProperty(
                created, key: kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
                allocator: kCFAllocatorDefault, valueOut: UnsafeMutableRawPointer($0))
        }
        usesHardware = queried == noErr && hardware.map { CFBooleanGetValue($0.takeRetainedValue()) } == true

        session = created
        format = description
        sps = newSPS
        pps = newPPS
    }

    private func makeSampleBuffer(avcc: [UInt8], format: CMVideoFormatDescription) throws -> CMSampleBuffer {
        var block: CMBlockBuffer?
        var status = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: avcc.count,
            blockAllocator: kCFAllocatorDefault, customBlockSource: nil, offsetToData: 0,
            dataLength: avcc.count, flags: 0, blockBufferOut: &block)
        guard status == noErr, let block else { throw DecodeError("CMBlockBufferCreateWithMemoryBlock", status: status) }
        status = avcc.withUnsafeBytes {
            CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block, offsetIntoDestination: 0,
                                          dataLength: avcc.count)
        }
        guard status == noErr else { throw DecodeError("CMBlockBufferReplaceDataBytes", status: status) }

        counter += 1
        var timing = CMSampleTimingInfo(duration: .invalid,
                                        presentationTimeStamp: CMTime(value: counter, timescale: 1000),
                                        decodeTimeStamp: .invalid)
        var size = avcc.count
        var sample: CMSampleBuffer?
        status = CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format, sampleCount: 1,
            sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleSizeEntryCount: 1,
            sampleSizeArray: &size, sampleBufferOut: &sample)
        guard status == noErr, let sample else { throw DecodeError("CMSampleBufferCreateReady", status: status) }
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
           CFArrayGetCount(attachments) > 0 {
            let dictionary = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(dictionary, Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                                 Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }
        return sample
    }

    /// Decodes one access unit. Returns the decoded NV12 buffer and the decode time in microseconds, or
    /// nil when the unit cannot be decoded yet (no parameter sets have arrived, or it holds no slices).
    func decode(_ unit: AccessUnit) throws -> (buffer: CVPixelBuffer, decodeUs: UInt64)? {
        if let newSPS = unit.sps, let newPPS = unit.pps, newSPS != sps || newPPS != pps || session == nil {
            try createSession(sps: newSPS, pps: newPPS)
        }
        guard let session, let format, !unit.slices.isEmpty else { return nil }

        let sample = try makeSampleBuffer(avcc: unit.avccPayload, format: format)
        let result = DecodeResult()
        let start = DispatchTime.now().uptimeNanoseconds
        var infoFlags = VTDecodeInfoFlags()
        let status = VTDecompressionSessionDecodeFrame(
            session, sampleBuffer: sample,
            flags: [._EnableAsynchronousDecompression, ._1xRealTimePlayback],
            infoFlagsOut: &infoFlags
        ) { status, _, imageBuffer, _, _ in
            result.complete(status: status, buffer: imageBuffer)
        }
        guard status == noErr else { throw DecodeError("VTDecompressionSessionDecodeFrame", status: status) }
        let waitStatus = VTDecompressionSessionWaitForAsynchronousFrames(session)
        guard waitStatus == noErr else { throw DecodeError("VTDecompressionSessionWaitForAsynchronousFrames", status: waitStatus) }
        let (outputStatus, buffer, finished) = result.take()
        guard outputStatus == noErr else { throw DecodeError("VideoToolbox decode", status: outputStatus) }
        guard let buffer else { return nil }
        let end = finished == 0 ? DispatchTime.now().uptimeNanoseconds : finished
        return (buffer, (end &- start) / 1000)
    }
}
