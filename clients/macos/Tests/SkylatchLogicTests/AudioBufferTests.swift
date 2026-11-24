// SPDX-License-Identifier: Apache-2.0
import XCTest
@testable import SkylatchLogic

final class AudioBufferTests: XCTestCase {
    private func write(_ buffer: inout PcmJitterBuffer, _ samples: [Int16]) {
        samples.withUnsafeBufferPointer { buffer.write($0) }
    }

    private func read(_ buffer: inout PcmJitterBuffer, frames: Int) -> (frames: Int, samples: [Int16]) {
        var out = [Int16](repeating: -1, count: frames * buffer.channels)
        let played = out.withUnsafeMutableBufferPointer { buffer.read(into: $0) }
        return (played, out)
    }

    func testWaitsForTheTargetBeforePlaying() {
        var buffer = PcmJitterBuffer(channels: 2, target: 4, limit: 8)
        write(&buffer, [1, 1, 2, 2, 3, 3])
        XCTAssertEqual(read(&buffer, frames: 2).frames, 0)
        XCTAssertEqual(read(&buffer, frames: 2).samples, [0, 0, 0, 0])
        write(&buffer, [4, 4])
        let first = read(&buffer, frames: 3)
        XCTAssertEqual(first.frames, 3)
        XCTAssertEqual(first.samples, [1, 1, 2, 2, 3, 3])
        XCTAssertEqual(buffer.queuedFrames, 1)
    }

    func testClearDropsQueuedSamplesAndPrimesAgain() {
        var buffer = PcmJitterBuffer(channels: 1, target: 2, limit: 4)
        write(&buffer, [1, 2, 3])
        buffer.clear()
        XCTAssertEqual(buffer.queuedFrames, 0)
        write(&buffer, [7])
        XCTAssertEqual(read(&buffer, frames: 1).frames, 0)
        write(&buffer, [8])
        XCTAssertEqual(read(&buffer, frames: 2).samples, [7, 8])
    }

    func testUnderrunPadsWithSilenceAndPrimesAgain() {
        var buffer = PcmJitterBuffer(channels: 1, target: 2, limit: 4)
        write(&buffer, [5, 6, 7])
        let drained = read(&buffer, frames: 4)
        XCTAssertEqual(drained.frames, 3)
        XCTAssertEqual(drained.samples, [5, 6, 7, 0])
        write(&buffer, [8])
        XCTAssertEqual(read(&buffer, frames: 1).frames, 0, "one frame is below the target again")
        write(&buffer, [9])
        XCTAssertEqual(read(&buffer, frames: 2).samples, [8, 9])
    }

    func testDropsTheOldestSamplesBeyondTheLimit() {
        var buffer = PcmJitterBuffer(channels: 2, target: 1, limit: 3)
        write(&buffer, [1, 1, 2, 2])
        write(&buffer, [3, 3, 4, 4])
        XCTAssertEqual(buffer.queuedFrames, 3)
        XCTAssertEqual(read(&buffer, frames: 3).samples, [2, 2, 3, 3, 4, 4])
        write(&buffer, (10..<20).map { Int16($0) })
        XCTAssertEqual(read(&buffer, frames: 3).samples, [14, 15, 16, 17, 18, 19])
    }

    func testIgnoresPartialFramesAndWrapsAround() {
        var buffer = PcmJitterBuffer(channels: 2, target: 1, limit: 2)
        write(&buffer, [1, 2, 3])
        XCTAssertEqual(buffer.queuedFrames, 1)
        for round in 0..<5 {
            let value = Int16(10 + round)
            write(&buffer, [value, -value])
            XCTAssertEqual(read(&buffer, frames: 1).samples, round == 0 ? [1, 2] : [value - 1, -(value - 1)])
        }
    }

    func testLowLatencyIsFortyMillisecondsWithAHundredAndTwentyMillisecondCap() {
        let buffer = PcmJitterBuffer.lowLatency()
        XCTAssertEqual(buffer.channels, 2)
        XCTAssertEqual(buffer.target, 48_000 * 40 / 1000)
        XCTAssertEqual(buffer.limit, 48_000 * 120 / 1000)
    }
}
