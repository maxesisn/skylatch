// SPDX-License-Identifier: Apache-2.0
import XCTest
@testable import SkylatchLogic

final class InputTests: XCTestCase {
    func testRecordLayout() {
        let r = InputRecord.encode(type: 6, code: 1, value: UInt32(bitPattern: Int32(-240)), x: 0x1234, y: 0xabcd,
                                   buttons: 0x0005, modifiers: 0x0102)
        XCTAssertEqual(r, [6, 0, 0, 1, 0xff, 0xff, 0xff, 0x10, 0x12, 0x34, 0xab, 0xcd, 0, 5, 1, 2])
        XCTAssertEqual(InputRecord.encode(type: 7), [7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
    }

    func testTextScalarValidity() {
        XCTAssertTrue(InputRecord.isValidTextScalar(0x4f60))
        XCTAssertTrue(InputRecord.isValidTextScalar(0x1f600))
        XCTAssertFalse(InputRecord.isValidTextScalar(0))
        XCTAssertFalse(InputRecord.isValidTextScalar(0xd800))
        XCTAssertFalse(InputRecord.isValidTextScalar(0x110000))
    }

    func testLetterbox() {
        XCTAssertEqual(Geometry.letterbox(width: 1000, height: 1000, videoWidth: 1920, videoHeight: 1080),
                       Viewport(x: 0, y: 219, width: 1000, height: 562))
        XCTAssertEqual(Geometry.letterbox(width: 2000, height: 500, videoWidth: 1000, videoHeight: 500),
                       Viewport(x: 500, y: 0, width: 1000, height: 500))
        XCTAssertEqual(Geometry.letterbox(width: 0, height: 10, videoWidth: 4, videoHeight: 3), .empty)
        XCTAssertEqual(Geometry.letterbox(width: 100, height: 100, videoWidth: 0, videoHeight: 0), .empty)
    }

    func testNormalize() {
        XCTAssertEqual(Geometry.normalize(0, start: 0, end: 100), 0)
        XCTAssertEqual(Geometry.normalize(99, start: 0, end: 100), 65535)
        XCTAssertEqual(Geometry.normalize(-50, start: 0, end: 100), 0)
        XCTAssertEqual(Geometry.normalize(500, start: 0, end: 100), 65535)
        XCTAssertEqual(Geometry.normalize(5, start: 5, end: 6), 0)
    }

    func testWheelDirectionAndAccumulation() {
        var wheel = WheelAccumulator()
        // AppKit content-direction deltas: scrolling content up (positive notch on the wire is down).
        XCTAssertEqual(wheel.wireNotches(scrollingDelta: -1, precise: false), 1)
        XCTAssertEqual(wheel.wireNotches(scrollingDelta: 3, precise: false), -3)
        // Precise deltas accumulate until a full notch.
        XCTAssertEqual(wheel.wireNotches(scrollingDelta: 8, precise: true), 0)
        XCTAssertEqual(wheel.wireNotches(scrollingDelta: 8, precise: true), 0)
        XCTAssertEqual(wheel.wireNotches(scrollingDelta: 8, precise: true), -1)
        // Reversing direction drops the stale remainder.
        wheel.reset()
        XCTAssertEqual(wheel.wireNotches(scrollingDelta: -10, precise: true), 0)
        XCTAssertEqual(wheel.wireNotches(scrollingDelta: 10, precise: true), 0)
        XCTAssertEqual(wheel.wireNotches(scrollingDelta: 10, precise: true), -1)
    }

    func testWheelChunksStayWithinProtocolLimit() {
        XCTAssertEqual(WheelAccumulator.chunks(3), [360])
        XCTAssertEqual(WheelAccumulator.chunks(-25), [-1200, -1200, -600])
        XCTAssertTrue(WheelAccumulator.chunks(0).isEmpty)
    }
}
