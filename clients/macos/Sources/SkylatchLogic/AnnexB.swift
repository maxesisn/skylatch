// SPDX-License-Identifier: Apache-2.0
import Foundation

/// H.264 NAL unit types the decoder cares about.
public enum NALType {
    public static let slice: UInt8 = 1
    public static let idr: UInt8 = 5
    public static let sei: UInt8 = 6
    public static let sps: UInt8 = 7
    public static let pps: UInt8 = 8
    public static let accessUnitDelimiter: UInt8 = 9
}

/// One access unit split into the parts a VideoToolbox decoder needs.
public struct AccessUnit: Equatable {
    /// Newest SPS NAL (without start code), if the access unit carries one.
    public var sps: [UInt8]?
    /// Newest PPS NAL (without start code), if the access unit carries one.
    public var pps: [UInt8]?
    /// Coded slice NALs (types 1...5), without start codes, in stream order.
    public var slices: [[UInt8]]
    /// True when any slice is an IDR picture.
    public var hasIDR: Bool

    public init(sps: [UInt8]? = nil, pps: [UInt8]? = nil, slices: [[UInt8]] = [], hasIDR: Bool = false) {
        self.sps = sps
        self.pps = pps
        self.slices = slices
        self.hasIDR = hasIDR
    }

    /// The slice NALs as one AVCC payload: each NAL prefixed by its 4-byte big-endian length.
    public var avccPayload: [UInt8] { AnnexB.avcc(from: slices) }
}

public enum AnnexB {
    /// Splits an Annex B byte stream into NAL unit ranges (start codes excluded).
    ///
    /// Handles 3-byte (`00 00 01`) and 4-byte (`00 00 00 01`) start codes. Zero bytes that precede a
    /// start code, or end the stream, are not part of any NAL unit. Bytes before the first start code
    /// are ignored.
    public static func nalRanges(in data: [UInt8]) -> [Range<Int>] {
        let n = data.count
        var starts: [(code: Int, payload: Int)] = []  // start-code position, first NAL byte
        var i = 0
        while i + 2 < n {
            if data[i + 2] > 1 {
                i += 3
            } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                starts.append((i, i + 3))
                i += 3
            } else {
                i += 1
            }
        }
        var ranges: [Range<Int>] = []
        for (k, s) in starts.enumerated() {
            var end = k + 1 < starts.count ? starts[k + 1].code : n
            // A 4-byte start code (or trailing_zero_8bits) leaves zero bytes before the next code.
            while end > s.payload && data[end - 1] == 0 { end -= 1 }
            if end > s.payload { ranges.append(s.payload..<end) }
        }
        return ranges
    }

    /// `nal_unit_type` of a NAL unit (low 5 bits of its first byte).
    public static func nalType(_ nal: [UInt8]) -> UInt8? {
        nal.first.map { $0 & 0x1f }
    }

    /// Parses one Annex B access unit.
    public static func parse(_ data: [UInt8]) -> AccessUnit {
        var unit = AccessUnit()
        for range in nalRanges(in: data) {
            let nal = Array(data[range])
            switch nalType(nal) {
            case NALType.sps?:
                unit.sps = nal
            case NALType.pps?:
                unit.pps = nal
            case let t? where t >= NALType.slice && t <= NALType.idr:
                unit.slices.append(nal)
                if t == NALType.idr { unit.hasIDR = true }
            default:
                break  // SEI, AUD, end of sequence, filler, ...
            }
        }
        return unit
    }

    /// Converts NAL units into AVCC form: 4-byte big-endian length, then the NAL bytes.
    public static func avcc(from nals: [[UInt8]]) -> [UInt8] {
        var out: [UInt8] = []
        out.reserveCapacity(nals.reduce(0) { $0 + $1.count + 4 })
        for nal in nals {
            let length = UInt32(nal.count)
            out.append(UInt8(truncatingIfNeeded: length >> 24))
            out.append(UInt8(truncatingIfNeeded: length >> 16))
            out.append(UInt8(truncatingIfNeeded: length >> 8))
            out.append(UInt8(truncatingIfNeeded: length))
            out.append(contentsOf: nal)
        }
        return out
    }
}
