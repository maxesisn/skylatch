// SPDX-License-Identifier: Apache-2.0
import XCTest
@testable import SkylatchLogic

final class KeyMapTests: XCTestCase {
    func testLettersAndDigits() {
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x00), 4)    // kVK_ANSI_A
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x06), 29)   // kVK_ANSI_Z
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x12), 30)   // kVK_ANSI_1
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x1d), 39)   // kVK_ANSI_0
    }

    func testControlKeys() {
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x24), 40)   // Return
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x35), 41)   // Escape
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x33), 42)   // Delete (backspace)
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x30), 43)   // Tab
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x31), 44)   // Space
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x39), 57)   // Caps Lock
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x75), 76)   // Forward Delete
    }

    func testFunctionNavigationAndKeypad() {
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x7a), 58)   // F1
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x6f), 69)   // F12
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x69), 104)  // F13
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x7e), 82)   // Up
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x7d), 81)   // Down
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x7b), 80)   // Left
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x7c), 79)   // Right
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x73), 74)   // Home
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x77), 77)   // End
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x4c), 88)   // Keypad Enter
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x52), 98)   // Keypad 0
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x6e), 101)  // Application (context menu)
    }

    func testModifiersLeftAndRight() {
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x3b), 224)  // Left Control
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x38), 225)  // Left Shift
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x3a), 226)  // Left Option
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x37), 227)  // Left Command
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x3e), 228)  // Right Control
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x3c), 229)  // Right Shift
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x3d), 230)  // Right Option
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x36), 231)  // Right Command
    }

    func testUnmappedKeys() {
        XCTAssertNil(KeyMap.hid(forVirtualKey: 0x3f))   // Fn
        XCTAssertNil(KeyMap.hid(forVirtualKey: 0x4a))   // Mute
        XCTAssertNil(KeyMap.hid(forVirtualKey: 0x100))
        XCTAssertNil(KeyMap.virtualKey(forHID: 0))
        XCTAssertNil(KeyMap.virtualKey(forHID: 72))     // Pause has no macOS key
    }

    func testBackslashAliasesPreferUsageFortyNine() {
        XCTAssertEqual(KeyMap.virtualKey(forHID: 49), 0x2a)
        XCTAssertEqual(KeyMap.virtualKey(forHID: 50), 0x2a)
        XCTAssertEqual(KeyMap.hid(forVirtualKey: 0x2a), 49)
    }

    func testRoundTripForEveryMappedKey() {
        for pair in KeyMap.pairs {
            let hid = KeyMap.hid(forVirtualKey: UInt16(pair.vk))
            XCTAssertNotNil(hid)
            XCTAssertEqual(KeyMap.virtualKey(forHID: hid!), pair.vk)
        }
        XCTAssertEqual(Set(KeyMap.pairs.map { $0.vk }).count, KeyMap.pairs.count - 1) // only 49/50 share a key
    }

    func testModifierDeviceFlags() {
        XCTAssertEqual(KeyMap.modifierIsDown(virtualKey: 0x38, rawFlags: 0x2 | 0x20102), true)
        XCTAssertEqual(KeyMap.modifierIsDown(virtualKey: 0x3c, rawFlags: 0x2 | 0x20102), false)
        XCTAssertEqual(KeyMap.modifierIsDown(virtualKey: 0x3c, rawFlags: 0x4), true)
        XCTAssertEqual(KeyMap.modifierIsDown(virtualKey: 0x3e, rawFlags: 0x2001), true)
        XCTAssertEqual(KeyMap.modifierIsDown(virtualKey: 0x3b, rawFlags: 0x2001), true)
        XCTAssertEqual(KeyMap.modifierIsDown(virtualKey: 0x37, rawFlags: 0), false)
        XCTAssertNil(KeyMap.modifierIsDown(virtualKey: 0x39, rawFlags: 0x10000))
    }
}
