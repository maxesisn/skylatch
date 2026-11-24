// SPDX-License-Identifier: Apache-2.0
import AppKit
import Carbon.HIToolbox
import QuartzCore
import SkylatchLogic

/// Layer-backed Metal view that turns AppKit keyboard, pointer, scroll and IME events into NSL1 input.
final class VideoView: NSView, NSTextInputClient {
    let metalLayer = CAMetalLayer()

    /// Receives each encoded 16-byte input record.
    var sendRecord: (([UInt8]) -> Void)?
    /// Called on the main thread when the view's pixel size or scale changes.
    var onResize: (() -> Void)?
    /// Current video size in pixels; input is ignored until the first frame has arrived.
    var videoSize: () -> (width: Int, height: Int) = { (0, 0) }
    /// False after the session ended; all input is dropped.
    var inputEnabled = true

    private var held = Set<UInt8>()
    private var buttons: UInt16 = 0
    private var modifiers: UInt16 = 0
    private var lastX: UInt16 = 0
    private var lastY: UInt16 = 0
    private var wheelX = WheelAccumulator()
    private var wheelY = WheelAccumulator()

    // IME state.
    private var markedString = ""
    private var handlingKeyEvent = false
    private var pendingTexts: [String] = []
    private var didSetMarkedText = false
    private var didRunCommand = false

    private var remoteCursor: NSCursor = .arrow

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layerContentsRedrawPolicy = .never
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func makeBackingLayer() -> CALayer { metalLayer }
    override var isFlipped: Bool { true }
    override var isOpaque: Bool { true }
    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    // MARK: Layout and Retina scale

    private func updateDrawableSize() {
        let scale = window?.backingScaleFactor ?? NSScreen.main?.backingScaleFactor ?? 2
        metalLayer.contentsScale = scale
        metalLayer.drawableSize = CGSize(width: max(1, bounds.width * scale), height: max(1, bounds.height * scale))
        onResize?()
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        window?.acceptsMouseMovedEvents = true
        updateDrawableSize()
    }

    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        updateDrawableSize()
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        updateDrawableSize()
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        for area in trackingAreas { removeTrackingArea(area) }
        addTrackingArea(NSTrackingArea(rect: .zero,
                                       options: [.mouseMoved, .mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
                                       owner: self, userInfo: nil))
    }

    // MARK: Remote cursor

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: remoteCursor)
    }

    /// Installs the host's cursor. All-zero input restores the arrow; a fully transparent image hides it.
    func setRemoteCursor(width: Int, height: Int, xhot: Int, yhot: Int, rgba: [UInt8]?) {
        remoteCursor = VideoView.makeCursor(width: width, height: height, xhot: xhot, yhot: yhot, rgba: rgba) ?? .arrow
        window?.invalidateCursorRects(for: self)
        if let window, window.isKeyWindow || NSPointInRect(window.mouseLocationOutsideOfEventStream, frame) {
            remoteCursor.set()
        }
    }

    static func makeCursor(width: Int, height: Int, xhot: Int, yhot: Int, rgba: [UInt8]?) -> NSCursor? {
        guard let rgba, width > 0, height > 0, rgba.count == width * height * 4 else { return nil }
        var opaque = false
        var premultiplied = [UInt8](repeating: 0, count: rgba.count)
        for i in stride(from: 0, to: rgba.count, by: 4) {
            let a = Int(rgba[i + 3])
            if a != 0 { opaque = true }
            premultiplied[i] = UInt8((Int(rgba[i]) * a + 127) / 255)
            premultiplied[i + 1] = UInt8((Int(rgba[i + 1]) * a + 127) / 255)
            premultiplied[i + 2] = UInt8((Int(rgba[i + 2]) * a + 127) / 255)
            premultiplied[i + 3] = rgba[i + 3]
        }
        let hotspot = NSPoint(x: min(max(xhot, 0), width - 1), y: min(max(yhot, 0), height - 1))
        if !opaque {
            // Fully transparent: hide the pointer with a 1x1 clear cursor.
            let clear = NSImage(size: NSSize(width: 1, height: 1))
            return NSCursor(image: clear, hotSpot: .zero)
        }
        guard let provider = CGDataProvider(data: Data(premultiplied) as CFData),
              let image = CGImage(width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32,
                                  bytesPerRow: width * 4, space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                  bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
                                  provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
        else { return nil }
        let nsImage = NSImage(cgImage: image, size: NSSize(width: width, height: height))
        return NSCursor(image: nsImage, hotSpot: hotspot)
    }

    // MARK: Sending

    private var ready: Bool { inputEnabled && sendRecord != nil && videoSize().width > 0 }

    private func send(_ type: UInt8, code: UInt16 = 0, value: UInt32 = 0) {
        guard inputEnabled, let sendRecord else { return }
        let pointer = type >= InputType.pointerMove && type <= InputType.wheel
        let record = InputRecord.encode(
            type: type, code: code, value: value, x: pointer ? lastX : 0, y: pointer ? lastY : 0,
            buttons: type >= InputType.releaseAll ? 0 : buttons, modifiers: type >= InputType.releaseAll ? 0 : modifiers)
        sendRecord(record)
    }

    /// Forgets every held key and button and tells the host to release them.
    func releaseAll() {
        held.removeAll()
        buttons = 0
        modifiers = 0
        wheelX.reset()
        wheelY.reset()
        if hasMarkedText() { discardComposition() }
        send(InputType.releaseAll)
    }

    private func discardComposition() {
        markedString = ""
        inputContext?.discardMarkedText()
    }

    // MARK: Pointer

    private func updatePosition(_ event: NSEvent) {
        let p = convert(event.locationInWindow, from: nil)
        let (vw, vh) = videoSize()
        let r = Geometry.letterbox(width: Double(bounds.width), height: Double(bounds.height),
                                   videoWidth: Double(vw), videoHeight: Double(vh))
        lastX = Geometry.normalize(Double(p.x), start: r.x, end: r.maxX)
        lastY = Geometry.normalize(Double(p.y), start: r.y, end: r.maxY)
    }

    private func pointerMoved(_ event: NSEvent) {
        updatePosition(event)
        if ready { send(InputType.pointerMove) }
    }

    override func mouseMoved(with event: NSEvent) { pointerMoved(event) }
    override func mouseDragged(with event: NSEvent) { pointerMoved(event) }
    override func rightMouseDragged(with event: NSEvent) { pointerMoved(event) }
    override func otherMouseDragged(with event: NSEvent) { pointerMoved(event) }

    private func button(_ code: UInt16, down: Bool, event: NSEvent) {
        guard ready else { return }
        updatePosition(event)
        if down {
            window?.makeFirstResponder(self)
            buttons |= UInt16(1 << (code - 1))
        } else {
            buttons &= ~UInt16(1 << (code - 1))
        }
        send(down ? InputType.buttonDown : InputType.buttonUp, code: code)
    }

    override func mouseDown(with event: NSEvent) { button(1, down: true, event: event) }
    override func mouseUp(with event: NSEvent) { button(1, down: false, event: event) }
    override func rightMouseDown(with event: NSEvent) { button(3, down: true, event: event) }
    override func rightMouseUp(with event: NSEvent) { button(3, down: false, event: event) }
    override func otherMouseDown(with event: NSEvent) { if event.buttonNumber == 2 { button(2, down: true, event: event) } }
    override func otherMouseUp(with event: NSEvent) { if event.buttonNumber == 2 { button(2, down: false, event: event) } }

    override func scrollWheel(with event: NSEvent) {
        guard ready else { return }
        updatePosition(event)
        let precise = event.hasPreciseScrollingDeltas
        let vertical = wheelY.wireNotches(scrollingDelta: Double(event.scrollingDeltaY), precise: precise)
        let horizontal = wheelX.wireNotches(scrollingDelta: Double(event.scrollingDeltaX), precise: precise)
        for part in WheelAccumulator.chunks(vertical) { send(InputType.wheel, code: 0, value: UInt32(bitPattern: part)) }
        for part in WheelAccumulator.chunks(horizontal) { send(InputType.wheel, code: 1, value: UInt32(bitPattern: part)) }
    }

    // MARK: Keyboard

    private func remoteKey(_ code: UInt8, down: Bool) {
        guard inputEnabled, videoSize().width > 0, (down || held.contains(code)) else { return }
        if down { held.insert(code) } else { held.remove(code) }
        if code >= 224 {
            let bit = UInt16(1 << (code - 224))
            modifiers = down ? (modifiers | bit) : (modifiers & ~bit)
        }
        send(down ? InputType.keyDown : InputType.keyUp, code: UInt16(code))
    }

    private func sendText(_ text: String, fromIME: Bool) {
        for scalar in text.unicodeScalars {
            let n = scalar.value
            guard InputRecord.isValidTextScalar(n) else { continue }
            if n >= 0x80 || (fromIME && n >= 0x20 && n < 0x7f) { send(InputType.text, value: n) }
        }
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard event.type == .keyDown else { return false }
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask).subtracting([.capsLock, .numericPad, .function])
        if flags == [.command, .control] && event.keyCode == UInt16(kVK_ANSI_F) {
            if !event.isARepeat { window?.toggleFullScreen(nil) }
            return true
        }
        // Cmd+Q stays local so the viewer can always be quit. Every other Command shortcut goes to the host.
        guard flags.contains(.command), !(flags == .command && event.keyCode == UInt16(kVK_ANSI_Q)) else { return false }
        guard inputEnabled else { return true }
        if let hid = KeyMap.hid(forVirtualKey: event.keyCode) {
            // macOS withholds key-up events while Command is held, so a shortcut is a complete tap.
            remoteKey(hid, down: true)
            remoteKey(hid, down: false)
        }
        return true
    }

    override func keyDown(with event: NSEvent) {
        guard inputEnabled else { return }
        let hid = KeyMap.hid(forVirtualKey: event.keyCode)
        let shortcut = !event.modifierFlags.intersection([.command, .control]).isEmpty
        var consumed = false
        if !shortcut {
            let wasComposing = hasMarkedText()
            pendingTexts = []
            didSetMarkedText = false
            didRunCommand = false
            handlingKeyEvent = true
            let handled = inputContext?.handleEvent(event) ?? false
            handlingKeyEvent = false
            let texts = pendingTexts
            pendingTexts = []
            var typed = false
            for text in texts {
                // Committed IME text always goes out as text; layout text only when it is not plain ASCII.
                sendText(text, fromIME: wasComposing)
                if text.unicodeScalars.contains(where: { $0.value >= 0x80 }) { typed = true }
            }
            // An input method that swallowed the key without text or a command also consumes it.
            consumed = wasComposing || didSetMarkedText || typed || (handled && texts.isEmpty && !didRunCommand)
        }
        if !consumed, let hid { remoteKey(hid, down: true) }
    }

    override func keyUp(with event: NSEvent) {
        guard let hid = KeyMap.hid(forVirtualKey: event.keyCode) else { return }
        remoteKey(hid, down: false)
    }

    override func flagsChanged(with event: NSEvent) {
        guard inputEnabled else { return }
        let vk = event.keyCode
        if vk == KeyMap.capsLockVirtualKey {
            // Caps Lock reports a state toggle, not a press and release: send a tap.
            if let hid = KeyMap.hid(forVirtualKey: vk), !hasMarkedText() {
                remoteKey(hid, down: true)
                remoteKey(hid, down: false)
            }
            return
        }
        guard let hid = KeyMap.hid(forVirtualKey: vk),
              let down = KeyMap.modifierIsDown(virtualKey: vk, rawFlags: UInt(event.modifierFlags.rawValue)) else { return }
        if down && hasMarkedText() { return }
        remoteKey(hid, down: down)
    }

    // MARK: NSTextInputClient

    private func plainString(_ value: Any) -> String {
        if let s = value as? String { return s }
        if let a = value as? NSAttributedString { return a.string }
        return ""
    }

    func insertText(_ string: Any, replacementRange: NSRange) {
        let text = plainString(string)
        markedString = ""
        if handlingKeyEvent {
            pendingTexts.append(text)
        } else {
            // Text from outside a key event, e.g. the emoji picker or dictation.
            sendText(text, fromIME: true)
        }
    }

    func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        markedString = plainString(string)
        didSetMarkedText = true
    }

    func unmarkText() { markedString = "" }
    func hasMarkedText() -> Bool { !markedString.isEmpty }
    func selectedRange() -> NSRange { NSRange(location: NSNotFound, length: 0) }

    func markedRange() -> NSRange {
        markedString.isEmpty ? NSRange(location: NSNotFound, length: 0) : NSRange(location: 0, length: (markedString as NSString).length)
    }

    func attributedSubstring(forProposedRange range: NSRange, actualRange: NSRangePointer?) -> NSAttributedString? { nil }
    func validAttributesForMarkedText() -> [NSAttributedString.Key] { [] }
    func characterIndex(for point: NSPoint) -> Int { NSNotFound }

    /// Places the candidate window just below the local pointer, as the Windows client does.
    func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
        guard let window else { return .zero }
        var p = window.mouseLocationOutsideOfEventStream
        let content = window.contentView?.bounds ?? .zero
        p.x = min(max(p.x, 0), max(0, content.width - 1))
        p.y = min(max(p.y, 16), max(16, content.height))
        return window.convertToScreen(NSRect(x: p.x, y: p.y - 16, width: 1, height: 16))
    }

    override func doCommand(by selector: Selector) {
        // Navigation and editing commands are forwarded as physical keys; swallowing them avoids the alert beep.
        didRunCommand = true
    }
}
