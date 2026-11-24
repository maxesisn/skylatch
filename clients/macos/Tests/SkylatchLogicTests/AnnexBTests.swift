// SPDX-License-Identifier: Apache-2.0
import XCTest
@testable import SkylatchLogic

final class AnnexBTests: XCTestCase {
    let sps: [UInt8] = [0x67, 0x64, 0x00, 0x1f, 0xac, 0xd9]
    let pps: [UInt8] = [0x68, 0xeb, 0xe3, 0xcb]
    let idr: [UInt8] = [0x65, 0x88, 0x84, 0x00, 0x10]
    let slice: [UInt8] = [0x41, 0x9a, 0x24, 0x6c]

    func testThreeByteStartCodes() {
        let data: [UInt8] = [0, 0, 1] + sps + [0, 0, 1] + pps + [0, 0, 1] + idr
        let ranges = AnnexB.nalRanges(in: data)
        XCTAssertEqual(ranges.map { Array(data[$0]) }, [sps, pps, idr])
    }

    func testFourByteStartCodes() {
        let data: [UInt8] = [0, 0, 0, 1] + sps + [0, 0, 0, 1] + pps + [0, 0, 0, 1] + idr
        let ranges = AnnexB.nalRanges(in: data)
        XCTAssertEqual(ranges.map { Array(data[$0]) }, [sps, pps, idr])
    }

    func testMixedStartCodesAndTrailingZeros() {
        // 4-byte, 3-byte, then trailing_zero_8bits at the end of the stream.
        let data: [UInt8] = [0, 0, 0, 1] + sps + [0, 0, 1] + pps + [0, 0, 0, 1] + slice + [0, 0, 0]
        let nals = AnnexB.nalRanges(in: data).map { Array(data[$0]) }
        XCTAssertEqual(nals, [sps, pps, slice])
    }

    func testGarbageBeforeFirstStartCodeAndEmptyInput() {
        XCTAssertTrue(AnnexB.nalRanges(in: []).isEmpty)
        XCTAssertTrue(AnnexB.nalRanges(in: [1, 2, 3, 4]).isEmpty)
        let data: [UInt8] = [0xff, 0xee, 0, 0, 1] + slice
        XCTAssertEqual(AnnexB.nalRanges(in: data).map { Array(data[$0]) }, [slice])
    }

    func testEmulationPreventionBytesAreKept() {
        // 00 00 03 inside a NAL is not a start code.
        let nal: [UInt8] = [0x65, 0x00, 0x00, 0x03, 0x01, 0x80]
        let data: [UInt8] = [0, 0, 1] + nal
        XCTAssertEqual(AnnexB.nalRanges(in: data).map { Array(data[$0]) }, [nal])
    }

    func testKeyframeAccessUnitExtractsParameterSets() {
        let aud: [UInt8] = [0x09, 0xf0]
        let sei: [UInt8] = [0x06, 0x05, 0x01, 0xff, 0x80]
        let data: [UInt8] = [0, 0, 0, 1] + aud + [0, 0, 0, 1] + sps + [0, 0, 0, 1] + pps + [0, 0, 1] + sei + [0, 0, 0, 1] + idr
        let unit = AnnexB.parse(data)
        XCTAssertEqual(unit.sps, sps)
        XCTAssertEqual(unit.pps, pps)
        XCTAssertEqual(unit.slices, [idr])
        XCTAssertTrue(unit.hasIDR)
    }

    func testMultiNalAccessUnitKeepsSliceOrderAndAvccPrefixes() {
        let second: [UInt8] = [0x41, 0x01, 0x02]
        let data: [UInt8] = [0, 0, 1] + slice + [0, 0, 0, 1] + second
        let unit = AnnexB.parse(data)
        XCTAssertNil(unit.sps)
        XCTAssertNil(unit.pps)
        XCTAssertFalse(unit.hasIDR)
        XCTAssertEqual(unit.slices, [slice, second])
        let expected: [UInt8] = [0, 0, 0, UInt8(slice.count)] + slice + [0, 0, 0, UInt8(second.count)] + second
        XCTAssertEqual(unit.avccPayload, expected)
    }

    func testAvccLengthIsBigEndian() {
        let nal = [UInt8](repeating: 0x41, count: 0x1_0203 & 0xffff) // 0x0203 = 515 bytes
        let out = AnnexB.avcc(from: [nal])
        XCTAssertEqual(Array(out.prefix(4)), [0, 0, 0x02, 0x03])
        XCTAssertEqual(out.count, 4 + nal.count)
    }

    func testNalType() {
        XCTAssertEqual(AnnexB.nalType(sps), 7)
        XCTAssertEqual(AnnexB.nalType(pps), 8)
        XCTAssertEqual(AnnexB.nalType(idr), 5)
        XCTAssertEqual(AnnexB.nalType(slice), 1)
        XCTAssertNil(AnnexB.nalType([]))
    }
}
