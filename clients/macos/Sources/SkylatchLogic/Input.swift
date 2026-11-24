// SPDX-License-Identifier: Apache-2.0
import Foundation

/// NSL1 input record types.
public enum InputType {
    public static let keyDown: UInt8 = 1
    public static let keyUp: UInt8 = 2
    public static let pointerMove: UInt8 = 3
    public static let buttonDown: UInt8 = 4
    public static let buttonUp: UInt8 = 5
    public static let wheel: UInt8 = 6
    public static let releaseAll: UInt8 = 7
    public static let text: UInt8 = 8
}

public enum InputRecord {
    /// Encodes one 16-byte NSL1 input record (big-endian fields).
    public static func encode(type: UInt8, code: UInt16 = 0, value: UInt32 = 0, x: UInt16 = 0, y: UInt16 = 0,
                              buttons: UInt16 = 0, modifiers: UInt16 = 0) -> [UInt8] {
        [type, 0,
         UInt8(code >> 8), UInt8(code & 0xff),
         UInt8(truncatingIfNeeded: value >> 24), UInt8(truncatingIfNeeded: value >> 16),
         UInt8(truncatingIfNeeded: value >> 8), UInt8(truncatingIfNeeded: value),
         UInt8(x >> 8), UInt8(x & 0xff), UInt8(y >> 8), UInt8(y & 0xff),
         UInt8(buttons >> 8), UInt8(buttons & 0xff), UInt8(modifiers >> 8), UInt8(modifiers & 0xff)]
    }

    /// True for a Unicode scalar the wire accepts as text input.
    public static func isValidTextScalar(_ scalar: UInt32) -> Bool {
        scalar >= 1 && scalar <= 0x10ffff && !(scalar >= 0xd800 && scalar <= 0xdfff)
    }
}

/// An axis-aligned rectangle in view coordinates.
public struct Viewport: Equatable {
    public var x: Double, y: Double, width: Double, height: Double
    public init(x: Double, y: Double, width: Double, height: Double) {
        self.x = x; self.y = y; self.width = width; self.height = height
    }
    public static let empty = Viewport(x: 0, y: 0, width: 0, height: 0)
    public var maxX: Double { x + width }
    public var maxY: Double { y + height }
}

public enum Geometry {
    /// Largest rectangle with the video's aspect ratio that fits `width` x `height`, centered.
    public static func letterbox(width: Double, height: Double, videoWidth: Double, videoHeight: Double) -> Viewport {
        guard videoWidth > 0, videoHeight > 0, width > 0, height > 0 else { return .empty }
        var w = width, h = height
        if width * videoHeight > height * videoWidth {
            w = max(1, (height * videoWidth / videoHeight).rounded(.down))
        } else {
            h = max(1, (width * videoHeight / videoWidth).rounded(.down))
        }
        return Viewport(x: ((width - w) / 2).rounded(.down), y: ((height - h) / 2).rounded(.down), width: w, height: h)
    }

    /// Maps a coordinate inside `[start, end)` to `0...65535`, clamping to the range.
    public static func normalize(_ p: Double, start: Double, end: Double) -> UInt16 {
        guard end - start > 1 else { return 0 }
        let clamped = min(max(p, start), end - 1)
        return UInt16(min(65535.0, ((clamped - start) * 65535.0 / (end - start - 1)).rounded(.down)))
    }
}

/// Converts scroll deltas into wire wheel steps (multiples of 120).
public struct WheelAccumulator {
    /// Points of precise (trackpad) scrolling that make one 120-unit notch.
    public static let pointsPerNotch = 20.0
    private var remainder = 0.0

    public init() {}

    public mutating func reset() { remainder = 0 }

    /// Feeds a signed delta in notch units and returns whole notches to send (positive or negative).
    private mutating func take(_ notches: Double) -> Int {
        if (notches < 0 && remainder > 0) || (notches > 0 && remainder < 0) { remainder = 0 }
        remainder += notches
        let whole = Int(remainder.rounded(.towardZero))
        remainder -= Double(whole)
        return whole
    }

    /// `delta` is an `NSEvent.scrollingDelta` axis value. The wire is positive for down/right, the
    /// opposite of AppKit's content-direction deltas, so the sign is flipped here.
    public mutating func wireNotches(scrollingDelta delta: Double, precise: Bool) -> Int {
        if precise { return take(-delta / Self.pointsPerNotch) }
        // Line-based wheels report whole lines (possibly accelerated); one line is one notch.
        return take(-delta)
    }

    /// Splits a notch count into wheel records of at most 10 notches (1200 units) each.
    public static func chunks(_ notches: Int) -> [Int32] {
        var left = notches
        var out: [Int32] = []
        while left != 0 {
            let part = max(-10, min(10, left))
            out.append(Int32(part * 120))
            left -= part
        }
        return out
    }
}
