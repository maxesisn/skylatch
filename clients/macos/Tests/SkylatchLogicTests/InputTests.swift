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
        // Precise deltas accumulate until a full notch (58 points).
        XCTAssertEqual(WheelAccumulator.pointsPerNotch, 58.0)
        XCTAssertEqual(wheel.wireNotches(scrollingDelta: 20, precise: true), 0)
        XCTAssertEqual(wheel.wireNotches(scrollingDelta: 20, precise: true), 0)
        XCTAssertEqual(wheel.wireNotches(scrollingDelta: 20, precise: true), -1)
        // Reversing direction drops the stale remainder.
        wheel.reset()
        XCTAssertEqual(wheel.wireNotches(scrollingDelta: -30, precise: true), 0)
        XCTAssertEqual(wheel.wireNotches(scrollingDelta: 30, precise: true), 0)
        XCTAssertEqual(wheel.wireNotches(scrollingDelta: 30, precise: true), -1)
    }

    func testTrackpadSwipeIsNotThreeTimesTooFast() {
        // A 580-point swipe is ten notches, not the 29 the previous 20-point constant produced.
        var wheel = WheelAccumulator()
        var total = 0
        for _ in 0..<58 { total += wheel.wireNotches(scrollingDelta: -10, precise: true) }
        XCTAssertEqual(total, 10)
    }

    func testAspectFitInsideEightyFivePercentOfScreen() throws {
        let visible = (width: 1440.0, height: 875.0)
        let fit = try XCTUnwrap(Geometry.aspectFit(videoWidth: 1920, videoHeight: 1080,
                                                   maxWidth: visible.width * 0.85, maxHeight: visible.height * 0.85,
                                                   minWidth: 320, minHeight: 200))
        XCTAssertEqual(fit.width, 1224)
        XCTAssertEqual(fit.height, 688.5, accuracy: 0.0001)
        // Height-bound: a portrait stream on the same screen.
        let tall = try XCTUnwrap(Geometry.aspectFit(videoWidth: 1080, videoHeight: 1920, maxWidth: 1224, maxHeight: 743.75))
        XCTAssertEqual(tall.height, 743)
        XCTAssertEqual(tall.width, 417.9375, accuracy: 0.0001)
        // The ratio is exact, and the result stays inside the 85% bounds.
        XCTAssertEqual(fit.width / fit.height, 1920.0 / 1080.0, accuracy: 1e-9)
        XCTAssertLessThanOrEqual(fit.width, visible.width * 0.85)
        XCTAssertLessThanOrEqual(fit.height, visible.height * 0.85)
    }

    func testAspectFitExactRatioAndMinimumClamp() throws {
        let exact = try XCTUnwrap(Geometry.aspectFit(videoWidth: 4, videoHeight: 3, maxWidth: 1000, maxHeight: 1000))
        XCTAssertEqual(exact.width, 1000)
        XCTAssertEqual(exact.height, 750, accuracy: 0.0001)
        let tiny = try XCTUnwrap(Geometry.aspectFit(videoWidth: 1920, videoHeight: 1080, maxWidth: 100, maxHeight: 100,
                                                    minWidth: 320, minHeight: 200))
        XCTAssertEqual(tiny.width, 320)
        XCTAssertEqual(tiny.height, 200)
        XCTAssertNil(Geometry.aspectFit(videoWidth: 0, videoHeight: 0, maxWidth: 100, maxHeight: 100))
        XCTAssertNil(Geometry.aspectFit(videoWidth: 16, videoHeight: 9, maxWidth: 0, maxHeight: 100))
    }

    func testRightCtrlDoubleTap() {
        var detector = DoubleTapDetector()
        let ms: UInt64 = 1_000_000
        XCTAssertFalse(detector.press(at: 1_000 * ms))
        XCTAssertTrue(detector.press(at: 1_300 * ms))
        // Disarmed after a success: the next press only arms.
        XCTAssertFalse(detector.press(at: 1_350 * ms))
        // Too slow.
        XCTAssertFalse(detector.press(at: 1_351 * ms + DoubleTapDetector.windowNanos))
        // Exactly on the window edge counts; an intervening key cancels.
        XCTAssertTrue(detector.press(at: 1_351 * ms + 2 * DoubleTapDetector.windowNanos))
        XCTAssertFalse(detector.press(at: 5_000 * ms))
        detector.cancel()
        XCTAssertFalse(detector.press(at: 5_100 * ms))
    }

    func testCmdCtrlSwap() {
        XCTAssertEqual(ModifierSwap.map(227, enabled: true), 224)
        XCTAssertEqual(ModifierSwap.map(231, enabled: true), 228)
        XCTAssertEqual(ModifierSwap.map(224, enabled: true), 227)
        XCTAssertEqual(ModifierSwap.map(228, enabled: true), 231)
        for code in [UInt8(4), 225, 226, 229, 230, 41] {
            XCTAssertEqual(ModifierSwap.map(code, enabled: true), code)
        }
        for code in [UInt8(224), 227, 228, 231] { XCTAssertEqual(ModifierSwap.map(code, enabled: false), code) }
        // The swap is an involution.
        for code in UInt8(0)...UInt8(255) {
            XCTAssertEqual(ModifierSwap.map(ModifierSwap.map(code, enabled: true), enabled: true), code)
        }
    }

    func testCmdCtrlSwapDefaults() {
        XCTAssertTrue(ModifierSwap.resolve(flag: nil, deviceOS: "windows"))
        XCTAssertTrue(ModifierSwap.resolve(flag: nil, deviceOS: "Linux"))
        XCTAssertFalse(ModifierSwap.resolve(flag: nil, deviceOS: "macos"))
        XCTAssertFalse(ModifierSwap.resolve(flag: nil, deviceOS: nil))
        XCTAssertFalse(ModifierSwap.resolve(flag: false, deviceOS: "windows"))
        XCTAssertTrue(ModifierSwap.resolve(flag: true, deviceOS: "macos"))
    }

    func testMonitorList() throws {
        let json = """
        [{"id":"a","name":"Display 1","x":0,"y":0,"width":1920,"height":1080,"primary":true,"active":true},
         {"id":"b","name":"Display\\u001b 2","x":1920,"y":0,"width":1280,"height":720,"primary":false,"active":false}]
        """
        let list = try XCTUnwrap(Monitors.parse(json: Data(json.utf8)))
        XCTAssertEqual(list, [MonitorEntry(id: "a", name: "Display 1", active: true),
                              MonitorEntry(id: "b", name: "Display  2", active: false)])
        XCTAssertEqual(Monitors.next(after: list)?.id, "b")
        let second = [list[0].withActive(false), list[1].withActive(true)]
        XCTAssertEqual(Monitors.next(after: second)?.id, "a")
        XCTAssertNil(Monitors.next(after: [list[0]]))
        XCTAssertNil(Monitors.parse(json: Data("{}".utf8)))
        XCTAssertNil(Monitors.parse(json: Data("[{\"id\":\"\",\"name\":\"x\"}]".utf8)))
    }

    func testHudStatsLine() {
        XCTAssertEqual(HudFormat.stats(width: 1920, height: 1080, fps: 59.6, latencyMs: 3.44), "1920x1080\u{2003}60 FPS\u{2003}3.4 ms")
        XCTAssertEqual(HudFormat.stats(width: 0, height: 0, fps: 0, latencyMs: 0), "")
    }

    func testWheelChunksStayWithinProtocolLimit() {
        XCTAssertEqual(WheelAccumulator.chunks(3), [360])
        XCTAssertEqual(WheelAccumulator.chunks(-25), [-1200, -1200, -600])
        XCTAssertTrue(WheelAccumulator.chunks(0).isEmpty)
    }
}

private extension MonitorEntry {
    func withActive(_ value: Bool) -> MonitorEntry { MonitorEntry(id: id, name: name, active: value) }
}
