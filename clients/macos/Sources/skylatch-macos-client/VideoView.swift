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
    /// True once the host accepted smooth-wheel records for this session.
    var smoothWheel: () -> Bool = { false }
    /// False after the session ended; all input is dropped.
    var inputEnabled = true
    /// In-window HUD over the video; pointer events it owns are never forwarded to the host.
    var hud: HudOverlay?
    /// Draggable performance readout; the pointer over it stays local too.
    var perfHud: PerfHudView?
    /// Cross-OS swap: local Cmd drives the remote Ctrl and local Ctrl drives the remote GUI key.
    var swapCmdCtrl = false
    /// Called when Right Ctrl is double-tapped (toggle system key capture).
    var onToggleCapture: (() -> Void)?
    /// Called when Esc has been held long enough in fullscreen (leave fullscreen).
    var onEscapeHold: (() -> Void)?
    /// Called on Cmd+Shift+O (step the performance HUD through hidden, card and strip).
    var onTogglePerf: (() -> Void)?
    /// How long Esc must stay down in fullscreen before it leaves fullscreen.
    static let escapeHoldSeconds = 1.2

    private var held = Set<UInt8>()
    private var buttons: UInt16 = 0
    private var modifiers: UInt16 = 0
    private var lastX: UInt16 = 0
    private var lastY: UInt16 = 0
    private var scroll = ScrollEncoder()
    private var rightCtrlTap = DoubleTapDetector()
    private var escapeTimer: Timer?
    /// Key codes whose key-down the system-key tap consumed, so their key-up is consumed too.
    private var capturedKeys = Set<UInt16>()

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
        perfHud?.relayout()
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
        let pointer = InputType.hasPosition(type), bare = InputType.isBare(type)
        let record = InputRecord.encode(
            type: type, code: code, value: value, x: pointer ? lastX : 0, y: pointer ? lastY : 0,
            buttons: bare ? 0 : buttons, modifiers: bare ? 0 : modifiers)
        sendRecord(record)
    }

    /// Forgets every held key and button and tells the host to release them.
    func releaseAll() {
        escapeTimer?.invalidate()
        escapeTimer = nil
        capturedKeys.removeAll()
        held.removeAll()
        buttons = 0
        modifiers = 0
        scroll.reset()
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
        // The HUD owns the top strip, the pill, the disconnect card and the performance readout; a drag in
        // progress is never hijacked.
        if buttons == 0 {
            let p = convert(event.locationInWindow, from: nil)
            let overHud = hud?.pointerMoved(to: p) == true
            if perfHud?.pointerMoved(to: p, blocked: overHud) == true || overHud { return }
        }
        updatePosition(event)
        if ready { send(InputType.pointerMove) }
    }

    override func mouseMoved(with event: NSEvent) { pointerMoved(event) }
    override func mouseExited(with event: NSEvent) {
        hud?.pointerLeft()
        perfHud?.pointerLeft()
    }
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
        let records = scroll.records(deltaX: Double(event.scrollingDeltaX), deltaY: Double(event.scrollingDeltaY),
                                     precise: event.hasPreciseScrollingDeltas, smooth: smoothWheel())
        for record in records { send(record.type, code: record.axis, value: UInt32(bitPattern: record.value)) }
    }

    // MARK: Keyboard

    private func remoteKey(_ hid: UInt8, down: Bool) {
        let code = ModifierSwap.map(hid, enabled: swapCmdCtrl)
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
        rightCtrlTap.cancel()
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask).subtracting([.capsLock, .numericPad, .function])
        if flags == [.command, .control] && event.keyCode == UInt16(kVK_ANSI_F) {
            if !event.isARepeat { window?.toggleFullScreen(nil) }
            return true
        }
        if flags == [.command, .shift] && event.keyCode == UInt16(kVK_ANSI_O) {
            if !event.isARepeat { onTogglePerf?() }
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
        if !inputEnabled {
            if event.keyCode == UInt16(kVK_Return), !event.isARepeat, hud?.isLostCardVisible == true { hud?.onAction?(.reconnect) }
            return
        }
        rightCtrlTap.cancel()
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
        if !consumed, let hid {
            remoteKey(hid, down: true)
            // A short Esc press has already gone out; only a long hold in fullscreen leaves fullscreen.
            if event.keyCode == UInt16(kVK_Escape), !event.isARepeat, isFullScreen { startEscapeHold() }
        }
    }

    override func keyUp(with event: NSEvent) {
        if event.keyCode == UInt16(kVK_Escape) { cancelEscapeHold() }
        guard let hid = KeyMap.hid(forVirtualKey: event.keyCode) else { return }
        remoteKey(hid, down: false)
    }

    private var isFullScreen: Bool { window?.styleMask.contains(.fullScreen) == true }

    private func startEscapeHold() {
        cancelEscapeHold()
        let timer = Timer(timeInterval: Self.escapeHoldSeconds, repeats: false) { [weak self] _ in
            guard let self else { return }
            self.escapeTimer = nil
            guard self.isFullScreen else { return }
            // Release Esc on the host before leaving, so it does not stay held there.
            self.remoteKey(UInt8(KeyMap.hid(forVirtualKey: UInt16(kVK_Escape)) ?? 41), down: false)
            self.onEscapeHold?()
        }
        RunLoop.main.add(timer, forMode: .common)
        escapeTimer = timer
    }

    private func cancelEscapeHold() {
        escapeTimer?.invalidate()
        escapeTimer = nil
    }

    /// Entry point for the system-key tap: Command and Control shortcuts that the system would otherwise
    /// act on are forwarded to the host as complete key presses. Returns true when the event was consumed.
    func handleCapturedKey(keyCode: UInt16, down: Bool, flags: NSEvent.ModifierFlags, isRepeat: Bool) -> Bool {
        guard inputEnabled, videoSize().width > 0, !hasMarkedText(),
              !KeyMap.isModifierVirtualKey(keyCode), let hid = KeyMap.hid(forVirtualKey: keyCode) else { return false }
        if !down {
            guard capturedKeys.remove(keyCode) != nil else { return false }
            remoteKey(hid, down: false)
            return true
        }
        let f = flags.intersection(.deviceIndependentFlagsMask).subtracting([.capsLock, .numericPad, .function])
        guard f.contains(.command) || f.contains(.control) else { return false }
        // The shortcuts that stay local without capture: quit, fullscreen and the performance HUD.
        if f == .command && keyCode == UInt16(kVK_ANSI_Q) { return false }
        if f == [.command, .control] && keyCode == UInt16(kVK_ANSI_F) { return false }
        if f == [.command, .shift] && keyCode == UInt16(kVK_ANSI_O) { return false }
        capturedKeys.insert(keyCode)
        rightCtrlTap.cancel()
        remoteKey(hid, down: true)
        return true
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
        if vk == UInt16(kVK_RightControl) {
            // Double-tap Right Ctrl toggles system key capture: the first tap goes to the host like any
            // Ctrl tap, the second is consumed here. Any other key press in between cancels the gesture.
            if down && rightCtrlTap.press(at: DispatchTime.now().uptimeNanoseconds) {
                onToggleCapture?()
                return
            }
        } else if down {
            rightCtrlTap.cancel()
        }
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
