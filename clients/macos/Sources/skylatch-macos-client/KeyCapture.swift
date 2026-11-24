// SPDX-License-Identifier: Apache-2.0
import AppKit
import ApplicationServices
import CoreGraphics

/// Session-wide keyboard tap that lets the viewer take system shortcuts (Cmd+Tab, Cmd+Space, Control+arrows
/// and the like) before the system acts on them. It needs the Accessibility permission; without it `start()`
/// fails and the viewer keeps its normal in-window keyboard handling.
final class KeyCapture {
    /// Receives a key event on the main thread. Returns true when the viewer consumed it.
    var handler: ((_ keyCode: UInt16, _ down: Bool, _ flags: NSEvent.ModifierFlags, _ isRepeat: Bool) -> Bool)?

    private var tap: CFMachPort?
    private var source: CFRunLoopSource?
    private var promptedForAccess = false

    var isRunning: Bool { tap != nil }

    /// Installs the tap. False when the system refuses it (Accessibility access missing); the first refusal
    /// also asks the system to show its permission prompt.
    @discardableResult
    func start() -> Bool {
        if tap != nil { return true }
        let mask: CGEventMask = (1 << CGEventType.keyDown.rawValue) | (1 << CGEventType.keyUp.rawValue)
        let callback: CGEventTapCallBack = { _, type, event, refcon in
            guard let refcon else { return Unmanaged.passUnretained(event) }
            let capture = Unmanaged<KeyCapture>.fromOpaque(refcon).takeUnretainedValue()
            return capture.receive(type: type, event: event) ? nil : Unmanaged.passUnretained(event)
        }
        guard let tap = CGEvent.tapCreate(tap: .cgSessionEventTap, place: .headInsertEventTap, options: .defaultTap,
                                          eventsOfInterest: mask, callback: callback,
                                          userInfo: Unmanaged.passUnretained(self).toOpaque()) else {
            if !promptedForAccess {
                promptedForAccess = true
                _ = AXIsProcessTrustedWithOptions(["AXTrustedCheckOptionPrompt": true] as CFDictionary)
            }
            return false
        }
        let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
        self.tap = tap
        self.source = source
        return true
    }

    func stop() {
        if let tap { CGEvent.tapEnable(tap: tap, enable: false) }
        if let source { CFRunLoopRemoveSource(CFRunLoopGetMain(), source, .commonModes) }
        tap = nil
        source = nil
    }

    deinit { stop() }

    /// Runs on the main run loop, where the tap's source is installed.
    private func receive(type: CGEventType, event: CGEvent) -> Bool {
        switch type {
        case .tapDisabledByTimeout, .tapDisabledByUserInput:
            if let tap { CGEvent.tapEnable(tap: tap, enable: true) }
            return false
        case .keyDown, .keyUp:
            let code = UInt16(truncatingIfNeeded: event.getIntegerValueField(.keyboardEventKeycode))
            let repeated = event.getIntegerValueField(.keyboardEventAutorepeat) != 0
            let flags = NSEvent.ModifierFlags(rawValue: UInt(event.flags.rawValue))
            return handler?(code, type == .keyDown, flags, repeated) ?? false
        default:
            return false
        }
    }
}
