// SPDX-License-Identifier: Apache-2.0
import XCTest
@testable import SkylatchLogic

final class CLITests: XCTestCase {
    let hex = String(repeating: "ab", count: 32)

    func testHelp() throws {
        XCTAssertTrue(try CLI.parse(["--help"]).help)
        XCTAssertTrue(try CLI.parse(["-h"]).help)
        XCTAssertThrowsError(try CLI.parse(["--help", "--list"]))
    }

    func testHubSessionOptions() throws {
        let c = try CLI.parse(["--hub", "wss://hub.example.com", "--token", "t", "--device", "d1", "--no-direct",
                               "--no-clipboard", "--smoke-seconds", "5"])
        XCTAssertEqual(c.hub, "wss://hub.example.com")
        XCTAssertEqual(c.device, "d1")
        XCTAssertTrue(c.noDirect && c.noClipboard)
        XCTAssertEqual(c.smokeSeconds, 5)
        XCTAssertTrue(c.isSmoke)
    }

    func testNoAudio() throws {
        let base = ["--hub", "wss://hub.example.com", "--token", "t"]
        XCTAssertFalse(try CLI.parse(base).noAudio)
        XCTAssertTrue(try CLI.parse(base + ["--no-audio"]).noAudio)
        XCTAssertTrue(try CLI.parse(["--direct", "ws://h", "--no-audio"]).noAudio)
        XCTAssertThrowsError(try CLI.parse(base + ["--no-audio", "--no-audio"]))
        XCTAssertThrowsError(try CLI.parse(base + ["--list", "--no-audio"]))
        XCTAssertTrue(CLI.usage.contains("--no-audio"))
    }

    func testHubList() throws {
        let c = try CLI.parse(["--hub", "ws://hub.example.com:8080", "--token", "t", "--list"])
        XCTAssertTrue(c.list)
        XCTAssertThrowsError(try CLI.parse(["--hub", "ws://h", "--token", "t", "--list", "--device", "x"]))
        XCTAssertThrowsError(try CLI.parse(["--hub", "ws://h", "--list"]))
    }

    func testDirect() throws {
        XCTAssertEqual(try CLI.parse(["--direct", "ws://192.0.2.1:17900"]).direct, "ws://192.0.2.1:17900")
        XCTAssertEqual(try CLI.parse(["--direct", "wss://h.example.com", "--token", hex]).token, hex)
        XCTAssertThrowsError(try CLI.parse(["--direct", "ws://h", "--token", "short"]))
        XCTAssertThrowsError(try CLI.parse(["--direct", "ws://h", "--list"]))
        XCTAssertThrowsError(try CLI.parse(["--direct", "ws://h", "--device", "d"]))
        XCTAssertThrowsError(try CLI.parse(["--direct", "ws://h", "--no-direct"]))
    }

    func testCmdCtrlSwapOptions() throws {
        let base = ["--hub", "wss://hub.example.com", "--token", "t"]
        XCTAssertNil(try CLI.parse(base).swapCmdCtrl)
        XCTAssertEqual(try CLI.parse(base + ["--swap-cmd-ctrl"]).swapCmdCtrl, true)
        XCTAssertEqual(try CLI.parse(base + ["--no-swap-cmd-ctrl"]).swapCmdCtrl, false)
        XCTAssertEqual(try CLI.parse(["--direct", "ws://h", "--swap-cmd-ctrl"]).swapCmdCtrl, true)
        XCTAssertThrowsError(try CLI.parse(base + ["--swap-cmd-ctrl", "--no-swap-cmd-ctrl"]))
        XCTAssertThrowsError(try CLI.parse(base + ["--swap-cmd-ctrl", "--swap-cmd-ctrl"]))
        XCTAssertThrowsError(try CLI.parse(base + ["--list", "--swap-cmd-ctrl"]))
        XCTAssertTrue(CLI.usage.contains("--swap-cmd-ctrl"))
    }

    func testInvalidArguments() {
        XCTAssertThrowsError(try CLI.parse([]))
        XCTAssertThrowsError(try CLI.parse(["--bogus"]))
        XCTAssertThrowsError(try CLI.parse(["--hub"]))
        XCTAssertThrowsError(try CLI.parse(["--hub", "--token", "t"]))
        XCTAssertThrowsError(try CLI.parse(["--hub", "http://h", "--token", "t"]))
        XCTAssertThrowsError(try CLI.parse(["--hub", "ws://user@h", "--token", "t"]))
        XCTAssertThrowsError(try CLI.parse(["--hub", "ws://h", "--direct", "ws://h", "--token", "t"]))
        XCTAssertThrowsError(try CLI.parse(["--direct", "ws://h", "--smoke-seconds", "0"]))
        XCTAssertThrowsError(try CLI.parse(["--direct", "ws://h", "--smoke-seconds", "x"]))
        XCTAssertThrowsError(try CLI.parse(["--direct", "ws://h", "--smoke-seconds", "86401"]))
        XCTAssertThrowsError(try CLI.parse(["--direct", "ws://h", "--no-clipboard", "--no-clipboard"]))
        XCTAssertThrowsError(try CLI.parse(["--direct", "ws://h", "--direct", "ws://i"]))
    }

    func testDeviceTable() throws {
        let json = """
        [{"device_id":"dev-1","name":"Desk\\u001b[2J","os":"macos","host_version":"0.1","monitors":2,
          "direct_endpoints":[{"kind":"lan","host":"192.0.2.10","port":7001},{"kind":"overlay","host":"2001:db8::1","port":7001}]},
         {"device_id":"d2","name":"Laptop","os":"windows","host_version":"0.1","monitors":1,"direct_endpoints":[]}]
        """
        let devices = try Devices.parse(json: Data(json.utf8))
        XCTAssertEqual(devices.count, 2)
        XCTAssertEqual(devices[0].name, "Desk [2J")
        XCTAssertEqual(devices[0].endpoints, "lan:192.0.2.10:7001, overlay:[2001:db8::1]:7001")
        XCTAssertEqual(devices[1].endpoints, "-")
        let lines = Devices.table(devices).split(separator: "\n").map(String.init)
        XCTAssertEqual(lines.count, 3)
        XCTAssertTrue(lines[0].hasPrefix("device_id  name"))
        XCTAssertTrue(lines[2].hasSuffix("-"))
        XCTAssertEqual(devices[1].pickerLabel, "Laptop  [d2]  windows")
        XCTAssertThrowsError(try Devices.parse(json: Data("{}".utf8)))
    }
}
