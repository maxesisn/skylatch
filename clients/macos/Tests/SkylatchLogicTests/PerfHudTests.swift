// SPDX-License-Identifier: Apache-2.0
import XCTest
@testable import SkylatchLogic

final class PerfHudTests: XCTestCase {
    private let second: UInt64 = 1_000_000_000
    private let ms: UInt64 = 1_000_000

    func testModeCyclesHiddenCardStrip() {
        XCTAssertEqual(PerfHudMode.hidden.next, .card)
        XCTAssertEqual(PerfHudMode.card.next, .strip)
        XCTAssertEqual(PerfHudMode.strip.next, .hidden)
    }

    func testRatesCountOnlyTheLastSecond() {
        var window = PerfWindow()
        let start = 10 * second
        // 60 frames of 25 000 bytes spread evenly over one second.
        for i in 0..<60 {
            let at = start + UInt64(i) * second / 60
            window.received(bytes: 25_000, at: at)
            window.decoded(decodeUs: 2_000, waitUs: 5_000, presented: true, at: at)
        }
        let full = window.rates(at: start + second - 1)
        XCTAssertEqual(full.fps, 60, accuracy: 1e-9)
        XCTAssertEqual(full.mbps, 12, accuracy: 1e-9)
        XCTAssertEqual(full.decodeMs!, 2, accuracy: 1e-9)
        XCTAssertEqual(full.waitMs!, 5, accuracy: 1e-9)

        // Half a second later the frames at or before the 0.5 s mark (31 of them) have left the window.
        let later = window.rates(at: start + 3 * second / 2)
        XCTAssertEqual(later.fps, 29, accuracy: 1e-9)
        XCTAssertEqual(later.mbps, 5.8, accuracy: 1e-9)

        let idle = window.rates(at: start + 3 * second)
        XCTAssertEqual(idle, PerfRates())
    }

    func testDecodedButNotPresentedFramesCountForDecodeOnly() {
        var window = PerfWindow()
        window.decoded(decodeUs: 1_000, waitUs: 9_000, presented: false, at: second)
        window.decoded(decodeUs: 3_000, waitUs: 4_000, presented: true, at: second + 10 * ms)
        let rates = window.rates(at: second + 20 * ms)
        XCTAssertEqual(rates.fps, 1, accuracy: 1e-9)
        XCTAssertEqual(rates.decodeMs!, 2, accuracy: 1e-9)
        XCTAssertEqual(rates.waitMs!, 4, accuracy: 1e-9)
    }

    func testAudioDelayIsAveragedAndExpires() {
        var window = PerfWindow()
        window.audioBuffered(ms: 30, at: second)
        window.audioBuffered(ms: 50, at: second + 10 * ms)
        XCTAssertEqual(window.rates(at: second + 20 * ms).audioMs!, 40, accuracy: 1e-9)
        XCTAssertNil(window.rates(at: 3 * second).audioMs)
    }

    func testResetForgetsEverything() {
        var window = PerfWindow()
        window.received(bytes: 1_000, at: second)
        window.decoded(decodeUs: 1, waitUs: 1, presented: true, at: second)
        window.audioBuffered(ms: 40, at: second)
        window.reset()
        XCTAssertEqual(window.rates(at: second), PerfRates())
    }

    func testEarlyTimestampsDoNotUnderflow() {
        var window = PerfWindow()
        window.received(bytes: 125_000, at: 5)
        XCTAssertEqual(window.rates(at: 10).mbps, 1, accuracy: 1e-9)
    }

    private func readout(audio: PerfAudio = .on, path: PerfPath = .direct, audioMs: Double? = 38) -> PerfReadout {
        PerfReadout(width: 3840, height: 2160, decoder: "VT", path: path, audio: audio,
                    rates: PerfRates(fps: 59.6, mbps: 24.34, decodeMs: 2.84, waitMs: 6.06, audioMs: audioMs))
    }

    func testCardRows() {
        XCTAssertEqual(PerfHudFormat.card(readout(), labels: .zh), [
            PerfRow(left: "60 FPS\u{2003}3840×2160", right: "直连"),
            PerfRow(left: "解 VT 2.8ms", right: "等 6.1ms"),
            PerfRow(left: "码率 24.3 Mbps", right: "音 Opus 38ms"),
        ])
        XCTAssertEqual(PerfHudFormat.card(readout(path: .relay), labels: .en)[0].right, "Relay")
        XCTAssertEqual(PerfHudFormat.card(readout(path: .unknown), labels: .zh)[0].right, "")
    }

    func testAudioStates() {
        XCTAssertEqual(PerfHudFormat.card(readout(audio: .muted), labels: .zh)[2].right, "音 已静音")
        XCTAssertEqual(PerfHudFormat.card(readout(audio: .off), labels: .zh)[2].right, "音 关闭")
        XCTAssertEqual(PerfHudFormat.card(readout(audioMs: nil), labels: .en)[2].right, "Audio No data")
    }

    func testStripLine() {
        XCTAssertEqual(PerfHudFormat.strip(readout(), labels: .zh),
                       "60 FPS\u{2003}3840×2160\u{2003}VT 2.8ms\u{2003}24.3 Mbps\u{2003}直连")
        XCTAssertEqual(PerfHudFormat.strip(PerfReadout(), labels: .en), "0 FPS\u{2003}--\u{2003}0.0 Mbps")
    }

    func testMissingMeasurementsShowDashes() {
        let rows = PerfHudFormat.card(PerfReadout(), labels: .zh)
        XCTAssertEqual(rows[0].left, "0 FPS")
        XCTAssertEqual(rows[1], PerfRow(left: "解 --", right: "等 --"))
        XCTAssertEqual(PerfHudFormat.mbps(150.4), "150 Mbps")
    }

    func testNoTextUsesAMiddleDot() {
        for labels in [PerfHudLabels.zh, .en] {
            for audio in [PerfAudio.on, .muted, .off] {
                let r = readout(audio: audio)
                let all = PerfHudFormat.card(r, labels: labels).flatMap { [$0.left, $0.right] } + [PerfHudFormat.strip(r, labels: labels)]
                XCTAssertFalse(all.contains { $0.contains("\u{00B7}") })
            }
        }
    }

    func testSlowStreamsAreFlagged() {
        XCTAssertTrue(PerfReadout(rates: PerfRates(fps: 29.4)).isSlow)
        XCTAssertFalse(PerfReadout(rates: PerfRates(fps: 30)).isSlow)
    }

    func testPlacementStaysInsideTheView() {
        let p = PerfHudPlacement.clamp(x: -20, y: 900, width: 214, height: 62, boundsWidth: 800, boundsHeight: 600)
        XCTAssertEqual(p.x, 0)
        XCTAssertEqual(p.y, 538)
        let q = PerfHudPlacement.clamp(x: 700, y: -1, width: 214, height: 62, boundsWidth: 800, boundsHeight: 600)
        XCTAssertEqual(q.x, 586)
        XCTAssertEqual(q.y, 0)
        // A view smaller than the HUD pins it to the top-left corner.
        let tiny = PerfHudPlacement.clamp(x: 50, y: 50, width: 214, height: 62, boundsWidth: 100, boundsHeight: 40)
        XCTAssertEqual(tiny.x, 0)
        XCTAssertEqual(tiny.y, 0)
    }

    func testDragKeepsTheGrabPointUnderThePointer() {
        let drag = PerfHudDrag(pointerX: 30, pointerY: 20, originX: 14, originY: 14)
        let moved = drag.origin(pointerX: 216, pointerY: 120, width: 214, height: 62, boundsWidth: 800, boundsHeight: 600)
        XCTAssertEqual(moved.x, 200)
        XCTAssertEqual(moved.y, 114)
        let clamped = drag.origin(pointerX: 2000, pointerY: -50, width: 214, height: 62, boundsWidth: 800, boundsHeight: 600)
        XCTAssertEqual(clamped.x, 586)
        XCTAssertEqual(clamped.y, 0)
    }
}
