// SPDX-License-Identifier: Apache-2.0
import Foundation

/// Mapping between macOS Carbon virtual key codes (`kVK_*`) and USB HID keyboard usages (page 0x07).
///
/// The pairs mirror the host's macOS backend (`host/src/backends/macos/keys.hpp` composed with the
/// shared HID-to-evdev table), so a key sent by this client is replayed as the same physical key.
public enum KeyMap {
    /// (HID usage, Carbon virtual key code) pairs.
    static let pairs: [(hid: UInt8, vk: UInt8)] = [
        // Letters A...Z
        (4, 0x00), (5, 0x0b), (6, 0x08), (7, 0x02), (8, 0x0e), (9, 0x03), (10, 0x05), (11, 0x04),
        (12, 0x22), (13, 0x26), (14, 0x28), (15, 0x25), (16, 0x2e), (17, 0x2d), (18, 0x1f),
        (19, 0x23), (20, 0x0c), (21, 0x0f), (22, 0x01), (23, 0x11), (24, 0x20), (25, 0x09),
        (26, 0x0d), (27, 0x07), (28, 0x10), (29, 0x06),
        // Digits 1...9, 0
        (30, 0x12), (31, 0x13), (32, 0x14), (33, 0x15), (34, 0x17), (35, 0x16), (36, 0x1a),
        (37, 0x1c), (38, 0x19), (39, 0x1d),
        // Return, Escape, Delete, Tab, Space, - = [ ] \ ; ' ` , . /
        (40, 0x24), (41, 0x35), (42, 0x33), (43, 0x30), (44, 0x31), (45, 0x1b), (46, 0x18),
        (47, 0x21), (48, 0x1e), (49, 0x2a), (50, 0x2a), (51, 0x29), (52, 0x27), (53, 0x32), (54, 0x2b),
        (55, 0x2f), (56, 0x2c), (57, 0x39),
        // F1...F12
        (58, 0x7a), (59, 0x78), (60, 0x63), (61, 0x76), (62, 0x60), (63, 0x61), (64, 0x62),
        (65, 0x64), (66, 0x65), (67, 0x6d), (68, 0x67), (69, 0x6f),
        // Navigation: Help (Insert), Home, PageUp, Forward Delete, End, PageDown, arrows
        (73, 0x72), (74, 0x73), (75, 0x74), (76, 0x75), (77, 0x77), (78, 0x79), (79, 0x7c),
        (80, 0x7b), (81, 0x7d), (82, 0x7e),
        // Keypad: Clear (NumLock), / * - + Enter 1...9 0 . and =
        (83, 0x47), (84, 0x4b), (85, 0x43), (86, 0x4e), (87, 0x45), (88, 0x4c), (89, 0x53),
        (90, 0x54), (91, 0x55), (92, 0x56), (93, 0x57), (94, 0x58), (95, 0x59), (96, 0x5b),
        (97, 0x5c), (98, 0x52), (99, 0x41),
        // ISO key, Application (context menu), keypad equals
        (100, 0x0a), (101, 0x6e), (103, 0x51),
        // F13...F20
        (104, 0x69), (105, 0x6b), (106, 0x71), (107, 0x6a), (108, 0x40), (109, 0x4f), (110, 0x50),
        (111, 0x5a),
        // Modifiers: LCtrl, LShift, LAlt(Option), LGUI(Command), RCtrl, RShift, RAlt, RGUI
        (224, 0x3b), (225, 0x38), (226, 0x3a), (227, 0x37), (228, 0x3e), (229, 0x3c), (230, 0x3d),
        (231, 0x36),
    ]

    private static let vkToHID: [UInt8: UInt8] = {
        var table: [UInt8: UInt8] = [:]
        // HID 49 and 50 both land on kVK_ANSI_Backslash; keep the first (49, Backslash).
        for pair in pairs where table[pair.vk] == nil { table[pair.vk] = pair.hid }
        return table
    }()

    private static let hidToVK: [UInt8: UInt8] = {
        var table: [UInt8: UInt8] = [:]
        for pair in pairs { table[pair.hid] = pair.vk }
        return table
    }()

    /// HID usage for a Carbon virtual key code, or nil when the key has no wire equivalent.
    public static func hid(forVirtualKey vk: UInt16) -> UInt8? {
        guard vk <= 0xff else { return nil }
        return vkToHID[UInt8(vk)]
    }

    /// Carbon virtual key code for a HID usage, or nil when the host has no macOS equivalent.
    public static func virtualKey(forHID hid: UInt8) -> UInt8? {
        hidToVK[hid]
    }

    public static let capsLockVirtualKey: UInt16 = 0x39

    /// Carbon codes of the eight modifier keys that arrive as `flagsChanged` events.
    public static func isModifierVirtualKey(_ vk: UInt16) -> Bool {
        modifierMasks[vk] != nil
    }

    // Device-dependent modifier bits in NSEvent.modifierFlags.rawValue (IOKit NX_DEVICE*KEYMASK).
    static let modifierMasks: [UInt16: UInt] = [
        0x3b: 0x0000_0001,  // left Control
        0x38: 0x0000_0002,  // left Shift
        0x3c: 0x0000_0004,  // right Shift
        0x37: 0x0000_0008,  // left Command
        0x36: 0x0000_0010,  // right Command
        0x3a: 0x0000_0020,  // left Option
        0x3d: 0x0000_0040,  // right Option
        0x3e: 0x0000_2000,  // right Control
    ]

    /// Whether a modifier key is down according to the raw flags of a `flagsChanged` event.
    /// Returns nil for keys that are not tracked left/right modifiers (including Caps Lock).
    public static func modifierIsDown(virtualKey vk: UInt16, rawFlags: UInt) -> Bool? {
        guard let mask = modifierMasks[vk] else { return nil }
        return rawFlags & mask != 0
    }
}
