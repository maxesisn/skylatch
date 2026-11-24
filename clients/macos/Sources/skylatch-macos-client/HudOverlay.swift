// SPDX-License-Identifier: Apache-2.0
import AppKit
import SkylatchLogic

/// Strings of the in-window viewer UI. Chinese for Chinese UI languages, English otherwise.
struct HudText {
    let monitor: String  // format with the 1-based display number
    let capture, swap, fullscreen, disconnect: String
    let captureOn, captureOff, captureNeedsAccess: String
    let fullscreenOn, fullscreenOff, swapOn, swapOff: String
    let lostTitle, reconnectingTitle: String  // format with the device name
    let reconnect, reconnecting, close, userDisconnected: String

    static let zh = HudText(
        monitor: "屏 %d", capture: "系统键", swap: "Cmd↔Ctrl", fullscreen: "全屏", disconnect: "断开",
        captureOn: "已捕获系统键，双按右 Ctrl 释放", captureOff: "已释放系统键，双按右 Ctrl 捕获",
        captureNeedsAccess: "捕获系统键需要在“系统设置 › 隐私与安全性 › 辅助功能”中允许本应用",
        fullscreenOn: "全屏已捕获系统键，长按 Esc 退出", fullscreenOff: "已退出全屏",
        swapOn: "Cmd ↔ Ctrl 互换已开启", swapOff: "Cmd ↔ Ctrl 互换已关闭",
        lostTitle: "与 %@ 的连接已中断", reconnectingTitle: "正在重新连接 %@…",
        reconnect: "立即重连", reconnecting: "重连中…", close: "关闭窗口", userDisconnected: "你已主动断开连接")
    static let en = HudText(
        monitor: "Screen %d", capture: "System keys", swap: "Cmd↔Ctrl", fullscreen: "Fullscreen", disconnect: "Disconnect",
        captureOn: "System keys captured. Double-tap Right Ctrl to release",
        captureOff: "System keys released. Double-tap Right Ctrl to capture",
        captureNeedsAccess: "Capturing system keys needs this app in System Settings › Privacy & Security › Accessibility",
        fullscreenOn: "Fullscreen with system keys captured. Hold Esc to exit", fullscreenOff: "Exited fullscreen",
        swapOn: "Cmd ↔ Ctrl swap on", swapOff: "Cmd ↔ Ctrl swap off",
        lostTitle: "Connection to %@ was lost", reconnectingTitle: "Reconnecting to %@…",
        reconnect: "Reconnect now", reconnecting: "Reconnecting…", close: "Close window", userDisconnected: "You disconnected")

    static let current: HudText = {
        let language = Locale.preferredLanguages.first ?? "en"
        return language.hasPrefix("zh") ? zh : en
    }()
}

/// In-window viewer UI pinned over the video: a top-edge latch strip that expands into a frosted control
/// pill, short toasts, and a disconnect / reconnect card. Everything under the strip, the pill or the card
/// is consumed here; only the rest of the window reaches the video view (and so the remote host).
final class HudOverlay: NSView {
    enum Action: Int {
        case none, surface, monitor, capture, swap, fullscreen, disconnect, reconnect, close
    }

    enum Card { case none, lost, reconnecting }

    /// Called on the main thread when a control is clicked.
    var onAction: ((Action) -> Void)?

    private struct Layout {
        var strip = NSRect.zero, trigger = NSRect.zero, pill = NSRect.zero, logo = NSRect.zero
        var name = NSRect.zero, stat = NSRect.zero, sep = NSRect.zero
        var buttons: [Action: NSRect] = [:]
        var showStat = false
        var card = NSRect.zero, icon = NSRect.zero, title = NSRect.zero, reason = NSRect.zero
        var reconnect = NSRect.zero, close = NSRect.zero
    }

    private static let pillHeight: CGFloat = 40, pillTop: CGFloat = 8
    private static let toastSeconds = 1.2, collapseSeconds = 1.8, expandSeconds = 0.18
    private static let cobalt = NSColor(hex: 0x2563EB)

    private let text = HudText.current
    private let pillBackdrop = HudOverlay.makeBackdrop()
    private let cardBackdrop = HudOverlay.makeBackdrop()
    private let canvas = HudCanvas()

    private var device = "", stat = ""
    private var monitorActive = 0, monitorCount = 0
    private var captureOn = false, swapOn = false, fullscreenOn = false
    private var card = Card.none
    private var reason = ""
    private var expandTarget = false, inside = false
    private var progress: CGFloat = 0
    private var leaveAt: TimeInterval?
    private var toastText = "", toastAt: TimeInterval?, toastAlpha: CGFloat = 0
    private var hover = Action.none, pressed = Action.none
    private var timer: Timer?
    private var lastStep: TimeInterval = 0

    override init(frame: NSRect) {
        super.init(frame: frame)
        autoresizingMask = [.width, .height]
        wantsLayer = true
        for backdrop in [pillBackdrop, cardBackdrop] {
            backdrop.isHidden = true
            addSubview(backdrop)
        }
        canvas.frame = bounds
        canvas.autoresizingMask = [.width, .height]
        canvas.drawHandler = { [weak self] in self?.drawContent() }
        addSubview(canvas)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var isFlipped: Bool { true }
    override var isOpaque: Bool { false }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    private static func makeBackdrop() -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = .hudWindow
        view.blendingMode = .withinWindow
        view.state = .active
        view.appearance = NSAppearance(named: .vibrantDark)
        view.wantsLayer = true
        view.layer?.masksToBounds = true
        return view
    }

    // MARK: State from the app

    func setDevice(_ name: String) { device = name; refresh() }
    func setStat(_ line: String) { stat = line; refresh() }

    func setMonitors(active: Int, count: Int) {
        monitorActive = active
        monitorCount = count
        refresh()
        window?.invalidateCursorRects(for: self)
    }

    func setFlags(capture: Bool, swap: Bool, fullscreen: Bool) {
        captureOn = capture; swapOn = swap; fullscreenOn = fullscreen
        refresh()
    }

    func toast(_ message: String) {
        toastText = message
        toastAt = ProcessInfo.processInfo.systemUptime
        toastAlpha = 0
        wake()
    }

    func showLost(_ why: String) {
        reason = why
        card = .lost
        expandTarget = false; progress = 0; leaveAt = nil; inside = false
        toastAt = nil; toastAlpha = 0
        hover = .none; pressed = .none
        cardChanged()
    }

    func showReconnecting() {
        card = .reconnecting
        hover = .none; pressed = .none
        cardChanged()
    }

    /// Drops the reconnecting card once the new session has a picture. A lost card stays.
    func dismissReconnecting() {
        guard card == .reconnecting else { return }
        card = .none
        hover = .none; pressed = .none
        cardChanged()
    }

    var isLostCardVisible: Bool { card == .lost }

    private func cardChanged() {
        refresh()
        window?.invalidateCursorRects(for: self)
    }

    private func refresh() {
        updateBackdrops()
        canvas.needsDisplay = true
    }

    // MARK: Layout

    private func measure(_ string: String, _ font: NSFont) -> CGFloat {
        ceil((string as NSString).size(withAttributes: [.font: font]).width)
    }

    private func monitorLabel() -> String { String(format: text.monitor, monitorActive + 1) }

    private func buttonLabel(_ action: Action) -> String {
        switch action {
        case .monitor: return monitorLabel()
        case .capture: return text.capture
        case .swap: return text.swap
        case .fullscreen: return text.fullscreen
        default: return text.disconnect
        }
    }

    private static let nameFont = NSFont.systemFont(ofSize: 13, weight: .semibold)
    private static let statFont = NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .regular)
    private static let buttonFont = NSFont.systemFont(ofSize: 12, weight: .regular)
    private static let toastFont = NSFont.systemFont(ofSize: 12.5)
    private static let titleFont = NSFont.systemFont(ofSize: 16, weight: .semibold)
    private static let bodyFont = NSFont.systemFont(ofSize: 12.5)
    private static let cardButtonFont = NSFont.systemFont(ofSize: 13.5, weight: .semibold)

    private func computeLayout() -> Layout {
        var l = Layout()
        let width = bounds.width, height = bounds.height, cx = width / 2
        l.strip = NSRect(x: cx - 28, y: -3, width: 56, height: 7)
        l.trigger = NSRect(x: cx - 120, y: 0, width: 240, height: 16)

        let padX: CGFloat = 10, gap: CGFloat = 8, btnGap: CGFloat = 4, logoSize: CGFloat = 22
        var actions: [Action] = []
        if monitorCount > 1 { actions.append(.monitor) }
        actions += [.capture, .swap, .fullscreen, .disconnect]
        func buttonWidth(_ a: Action) -> CGFloat { measure(buttonLabel(a), Self.buttonFont) + 22 }
        let buttonsWidth = actions.reduce(CGFloat(0)) { $0 + buttonWidth($1) } + btnGap * CGFloat(actions.count - 1)

        var nameWidth = min(measure(device, Self.nameFont), 150)
        let statWidth = measure(stat, Self.statFont)
        func total(_ withStat: Bool, _ n: CGFloat) -> CGFloat {
            padX + logoSize + gap + n + (withStat ? gap + statWidth : 0) + gap + 1 + gap + buttonsWidth + padX - 2
        }
        var withStat = statWidth > 0
        let available = max(120, width - 16)
        if total(withStat, nameWidth) > available { withStat = false }
        if total(withStat, nameWidth) > available { nameWidth = max(30, nameWidth - (total(withStat, nameWidth) - available)) }
        let pillWidth = total(withStat, nameWidth), pillLeft = cx - pillWidth / 2
        let top = Self.pillTop, h = Self.pillHeight
        l.pill = NSRect(x: pillLeft, y: top, width: pillWidth, height: h)
        var x = pillLeft + padX
        l.logo = NSRect(x: x, y: top + (h - logoSize) / 2, width: logoSize, height: logoSize)
        x += logoSize + gap
        l.name = NSRect(x: x, y: top, width: nameWidth, height: h)
        x += nameWidth
        if withStat {
            x += gap
            l.stat = NSRect(x: x, y: top, width: statWidth, height: h)
            x += statWidth
        }
        l.showStat = withStat
        x += gap
        l.sep = NSRect(x: x, y: top + 10, width: 1, height: h - 20)
        x += 1 + gap
        for a in actions {
            let w = buttonWidth(a)
            l.buttons[a] = NSRect(x: x, y: top + 6, width: w, height: h - 12)
            x += w + btnGap
        }

        let cw = min(400, max(120, width - 32)), ch: CGFloat = 216
        l.card = NSRect(x: cx - cw / 2, y: height / 2 - ch / 2, width: cw, height: ch)
        l.icon = NSRect(x: cx - 20, y: l.card.minY + 26, width: 40, height: 40)
        l.title = NSRect(x: l.card.minX + 20, y: l.card.minY + 78, width: cw - 40, height: 28)
        l.reason = NSRect(x: l.card.minX + 24, y: l.card.minY + 110, width: cw - 48, height: 40)
        let bw = (cw - 48 - 12) / 2, by = l.card.maxY - 24 - 36
        l.reconnect = NSRect(x: l.card.minX + 24, y: by, width: bw, height: 36)
        l.close = NSRect(x: l.reconnect.maxX + 12, y: by, width: bw, height: 36)
        return l
    }

    private func hit(_ l: Layout, _ p: NSPoint) -> Action {
        if card != .none {
            if card == .lost && l.reconnect.contains(p) { return .reconnect }
            if l.close.contains(p) { return .close }
            return .surface
        }
        if (expandTarget || progress > 0.01) && l.pill.contains(p) {
            for (action, rect) in l.buttons where rect.contains(p) { return action }
            return .surface
        }
        return l.trigger.contains(p) ? .surface : .none
    }

    private func ease(_ t: CGFloat) -> CGFloat { let u = 1 - min(max(t, 0), 1); return 1 - u * u * u }

    // MARK: Hit testing and pointer

    override func hitTest(_ point: NSPoint) -> NSView? {
        let p = convert(point, from: superview)
        guard bounds.contains(p) else { return nil }
        return hit(computeLayout(), p) == .none ? nil : self
    }

    /// Whether the HUD owns this point (view coordinates), so the video view must not forward it.
    func owns(_ p: NSPoint) -> Bool { hit(computeLayout(), p) != .none }

    /// Tracks the pointer for hover and auto-expand. True when the HUD owns the position.
    func pointerMoved(to p: NSPoint) -> Bool {
        let l = computeLayout()
        if card == .none && !expandTarget && l.trigger.contains(p) {
            expandTarget = true
            window?.invalidateCursorRects(for: self)
        }
        let action = hit(l, p)
        let over = card == .none && action != .none
        if over { leaveAt = nil } else if expandTarget && leaveAt == nil { leaveAt = ProcessInfo.processInfo.systemUptime }
        inside = over
        if action != hover { hover = action }
        wake()
        return action != .none
    }

    func pointerLeft() {
        inside = false
        hover = .none
        if expandTarget && leaveAt == nil { leaveAt = ProcessInfo.processInfo.systemUptime }
        wake()
    }

    private func local(_ event: NSEvent) -> NSPoint { convert(event.locationInWindow, from: nil) }

    override func mouseDown(with event: NSEvent) {
        let action = hit(computeLayout(), local(event))
        pressed = action
        if action == .surface && card == .none && !expandTarget {
            expandTarget = true
            window?.invalidateCursorRects(for: self)
        }
        hover = action
        wake()
    }

    override func mouseDragged(with event: NSEvent) {
        hover = hit(computeLayout(), local(event))
        canvas.needsDisplay = true
    }

    override func mouseMoved(with event: NSEvent) { _ = pointerMoved(to: local(event)) }

    override func mouseUp(with event: NSEvent) {
        let action = hit(computeLayout(), local(event))
        let started = pressed
        pressed = .none
        canvas.needsDisplay = true
        guard action == started, action.rawValue >= Action.monitor.rawValue else { return }
        // A click keeps the pill open a little longer so the result is visible.
        if card == .none { leaveAt = ProcessInfo.processInfo.systemUptime }
        onAction?(action)
    }

    // The HUD swallows every other pointer event over its controls and the card.
    override func rightMouseDown(with event: NSEvent) {}
    override func rightMouseUp(with event: NSEvent) {}
    override func rightMouseDragged(with event: NSEvent) {}
    override func otherMouseDown(with event: NSEvent) {}
    override func otherMouseUp(with event: NSEvent) {}
    override func otherMouseDragged(with event: NSEvent) {}
    override func scrollWheel(with event: NSEvent) {}

    override func resetCursorRects() {
        let l = computeLayout()
        if card != .none {
            addCursorRect(bounds, cursor: .arrow)
        } else if expandTarget || progress > 0.01 {
            addCursorRect(l.pill.union(l.trigger).intersection(bounds), cursor: .arrow)
        } else {
            addCursorRect(l.trigger.intersection(bounds), cursor: .arrow)
        }
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        refresh()
        window?.invalidateCursorRects(for: self)
    }

    // MARK: Animation

    private func wake() {
        canvas.needsDisplay = true
        updateBackdrops()
        guard timer == nil else { return }
        lastStep = ProcessInfo.processInfo.systemUptime
        let timer = Timer(timeInterval: 1.0 / 60.0, repeats: true) { [weak self] _ in self?.step() }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    private func step() {
        let now = ProcessInfo.processInfo.systemUptime
        let dt = CGFloat(min(max(now - lastStep, 0), 0.1))
        lastStep = now
        var busy = false
        if expandTarget && !inside, let leave = leaveAt, now - leave >= Self.collapseSeconds {
            expandTarget = false
            leaveAt = nil
            window?.invalidateCursorRects(for: self)
        }
        let goal: CGFloat = expandTarget ? 1 : 0
        if progress != goal {
            let delta = dt / CGFloat(Self.expandSeconds)
            progress = goal > progress ? min(goal, progress + delta) : max(goal, progress - delta)
            busy = true
        }
        if let start = toastAt {
            let t = now - start
            if t >= Self.toastSeconds {
                toastAt = nil
                toastAlpha = 0
            } else {
                toastAlpha = CGFloat(min(max(min(t / 0.15, (Self.toastSeconds - t) / 0.3), 0), 1))
                busy = true
            }
        }
        updateBackdrops()
        canvas.needsDisplay = true
        if !busy && !expandTarget && toastAt == nil {
            timer?.invalidate()
            timer = nil
            window?.invalidateCursorRects(for: self)
        }
    }

    private func updateBackdrops() {
        let l = computeLayout()
        if card != .none {
            pillBackdrop.isHidden = true
            cardBackdrop.isHidden = false
            cardBackdrop.frame = l.card
            cardBackdrop.layer?.cornerRadius = 16
            return
        }
        cardBackdrop.isHidden = true
        guard progress > 0.01 else {
            pillBackdrop.isHidden = true
            return
        }
        let e = ease(progress)
        func lerp(_ a: CGFloat, _ b: CGFloat) -> CGFloat { a + (b - a) * e }
        let strip = NSRect(x: l.strip.minX, y: 0, width: l.strip.width, height: 4)
        pillBackdrop.isHidden = false
        pillBackdrop.frame = NSRect(x: lerp(strip.minX, l.pill.minX), y: lerp(strip.minY, l.pill.minY),
                                    width: lerp(strip.width, l.pill.width), height: lerp(strip.height, l.pill.height))
        pillBackdrop.layer?.cornerRadius = lerp(2, Self.pillHeight / 2)
        pillBackdrop.alphaValue = e
    }

    // MARK: Drawing

    private func drawContent() {
        let l = computeLayout()
        if card != .none {
            drawCard(l)
            return
        }
        drawPill(l)
        drawToast(l)
    }

    private func rounded(_ r: NSRect, _ radius: CGFloat, fill: NSColor) {
        fill.setFill()
        NSBezierPath(roundedRect: r, xRadius: radius, yRadius: radius).fill()
    }

    private func outline(_ r: NSRect, _ radius: CGFloat, color: NSColor, width: CGFloat = 1) {
        let inset = r.insetBy(dx: width / 2, dy: width / 2)
        let path = NSBezierPath(roundedRect: inset, xRadius: max(0, radius - width / 2), yRadius: max(0, radius - width / 2))
        path.lineWidth = width
        color.setStroke()
        path.stroke()
    }

    private func draw(_ s: String, in r: NSRect, font: NSFont, color: NSColor, align: NSTextAlignment = .center,
                      wrap: Bool = false) {
        let style = NSMutableParagraphStyle()
        style.alignment = align
        style.lineBreakMode = wrap ? .byWordWrapping : .byTruncatingTail
        let attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: color, .paragraphStyle: style]
        if wrap {
            (s as NSString).draw(with: r, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine], attributes: attributes)
        } else {
            let height = ceil(font.ascender - font.descender)
            let box = NSRect(x: r.minX, y: r.midY - height / 2, width: r.width, height: height)
            (s as NSString).draw(with: box, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine], attributes: attributes)
        }
    }

    private func drawLogo(_ r: NSRect, alpha: CGFloat) {
        let u = r.width / 64
        func box(_ x0: CGFloat, _ y0: CGFloat, _ x1: CGFloat, _ y1: CGFloat, _ radius: CGFloat, _ color: NSColor) {
            rounded(NSRect(x: r.minX + x0 * u, y: r.minY + y0 * u, width: (x1 - x0) * u, height: (y1 - y0) * u),
                    radius * u, fill: color.withAlphaComponent(alpha))
        }
        let ink = NSColor(hex: 0x1A1A18), paper = NSColor(hex: 0xFCFBF8)
        box(1.5, 1.5, 62.5, 62.5, 15.5, ink)
        box(4.5, 4.5, 59.5, 59.5, 12.5, paper)
        box(14, 14, 50, 50, 0, ink)
        box(18, 18, 46, 46, 0, paper)
        box(24, 29, 48, 35, 0, Self.cobalt)
        box(20, 24, 24, 40, 0, ink)
    }

    private func drawPill(_ l: Layout) {
        let e = ease(progress)
        let stripColor = captureOn ? Self.cobalt.withAlphaComponent(0.96) : NSColor(hex: 0x8E8C85).withAlphaComponent(0.6)
        if e < 0.99 || progress < 1 {
            // Collapsed (or collapsing) strip, and the tint of the growing pill.
            func lerp(_ a: CGFloat, _ b: CGFloat) -> CGFloat { a + (b - a) * e }
            let cur = NSRect(x: lerp(l.strip.minX, l.pill.minX), y: lerp(l.strip.minY, l.pill.minY),
                             width: lerp(l.strip.width, l.pill.width), height: lerp(l.strip.height, l.pill.height))
            let radius = lerp(3.5, Self.pillHeight / 2)
            rounded(cur, radius, fill: stripColor.blended(withFraction: e, of: NSColor(hex: 0x1A1A18).withAlphaComponent(0.45)) ?? stripColor)
            outline(cur, radius, color: NSColor(hex: 0x1A1A18).withAlphaComponent(0.45 * (1 - e)))
            if e <= 0.55 { return }
        } else {
            rounded(l.pill, Self.pillHeight / 2, fill: NSColor(hex: 0x1A1A18).withAlphaComponent(0.45))
        }
        outline(l.pill, Self.pillHeight / 2, color: NSColor.white.withAlphaComponent(0.16 * e))
        let a = min(max((e - 0.55) / 0.45, 0), 1)
        drawLogo(l.logo, alpha: a)
        draw(device, in: l.name, font: Self.nameFont, color: NSColor(hex: 0xF4F2EC).withAlphaComponent(a), align: .left)
        if l.showStat {
            draw(stat, in: l.stat, font: Self.statFont, color: NSColor(hex: 0xA9A69C).withAlphaComponent(a), align: .left)
        }
        rounded(l.sep, 0, fill: NSColor.white.withAlphaComponent(0.14 * a))
        for (action, r) in l.buttons {
            let hot = hover == action, down = hot && pressed == action
            if action == .disconnect {
                let fill = down ? 0xB91C1C : hot ? 0xEF4444 : 0xDC2626
                rounded(r, 8, fill: NSColor(hex: UInt32(fill)).withAlphaComponent(a * (hot ? 1 : 0.9)))
                draw(text.disconnect, in: r, font: Self.buttonFont, color: NSColor.white.withAlphaComponent(a))
                continue
            }
            let on = (action == .capture && captureOn) || (action == .swap && swapOn) || (action == .fullscreen && fullscreenOn)
            if on {
                let fill = down ? 0x1D4ED8 : hot ? 0x3B73F0 : 0x2563EB
                rounded(r, 8, fill: NSColor(hex: UInt32(fill)).withAlphaComponent(a))
            } else {
                rounded(r, 8, fill: NSColor.white.withAlphaComponent((down ? 0.24 : hot ? 0.16 : 0.07) * a))
            }
            draw(buttonLabel(action), in: r, font: Self.buttonFont,
                 color: (on ? NSColor.white : NSColor(hex: 0xF4F2EC)).withAlphaComponent(a))
        }
    }

    private func drawToast(_ l: Layout) {
        guard toastAlpha > 0.01, !toastText.isEmpty else { return }
        let width = min(measure(toastText, Self.toastFont) + 32, max(80, bounds.width - 16))
        let top = progress > 0.5 ? Self.pillTop + Self.pillHeight + 8 : 14
        let r = NSRect(x: bounds.midX - width / 2, y: top, width: width, height: 32)
        rounded(r.offsetBy(dx: 0, dy: 2), 16, fill: NSColor.black.withAlphaComponent(0.18 * toastAlpha))
        rounded(r, 16, fill: NSColor(hex: 0x1A1A18).withAlphaComponent(0.92 * toastAlpha))
        outline(r, 16, color: NSColor.white.withAlphaComponent(0.14 * toastAlpha))
        draw(toastText, in: r, font: Self.toastFont, color: NSColor(hex: 0xF4F2EC).withAlphaComponent(toastAlpha))
    }

    private func drawCard(_ l: Layout) {
        let lost = card == .lost
        NSColor.black.withAlphaComponent(0.45).setFill()
        bounds.fill()
        rounded(l.card, 16, fill: NSColor(hex: 0x1A1A18).withAlphaComponent(0.35))
        outline(l.card, 16, color: NSColor.white.withAlphaComponent(0.18))
        let circle = l.icon
        (lost ? NSColor(hex: 0xDC2626) : Self.cobalt).setFill()
        NSBezierPath(ovalIn: circle).fill()
        NSColor.white.setFill()
        if lost {
            rounded(NSRect(x: circle.midX - 1.7, y: circle.midY - 10, width: 3.4, height: 13), 1.7, fill: .white)
            NSBezierPath(ovalIn: NSRect(x: circle.midX - 2.1, y: circle.midY + 6.9, width: 4.2, height: 4.2)).fill()
        } else {
            for i in -1...1 {
                NSBezierPath(ovalIn: NSRect(x: circle.midX + CGFloat(i) * 9 - 2.3, y: circle.midY - 2.3, width: 4.6, height: 4.6)).fill()
            }
        }
        draw(String(format: lost ? text.lostTitle : text.reconnectingTitle, device), in: l.title,
             font: Self.titleFont, color: .white)
        if lost {
            draw(reason, in: l.reason, font: Self.bodyFont, color: NSColor(hex: 0xC9C7BF), wrap: true)
        }
        let hotR = lost && hover == .reconnect, downR = hotR && pressed == .reconnect
        let primary: NSColor = !lost ? NSColor.white.withAlphaComponent(0.16)
            : NSColor(hex: downR ? 0x1E40AF : hotR ? 0x1D4ED8 : 0x2563EB)
        rounded(l.reconnect, 10, fill: primary)
        draw(lost ? text.reconnect : text.reconnecting, in: l.reconnect, font: Self.cardButtonFont,
             color: lost ? .white : NSColor.white.withAlphaComponent(0.5))
        let hotC = hover == .close, downC = hotC && pressed == .close
        if hotC { rounded(l.close, 10, fill: NSColor.white.withAlphaComponent(downC ? 0.2 : 0.1)) }
        outline(l.close, 10, color: NSColor.white.withAlphaComponent(0.4))
        draw(text.close, in: l.close, font: Self.cardButtonFont, color: .white)
    }
}

/// Transparent drawing surface for the HUD; it never takes a click.
private final class HudCanvas: NSView {
    var drawHandler: (() -> Void)?

    override var isFlipped: Bool { true }
    override var isOpaque: Bool { false }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func draw(_ dirtyRect: NSRect) { drawHandler?() }
}

extension NSColor {
    convenience init(hex: UInt32) {
        self.init(srgbRed: CGFloat((hex >> 16) & 255) / 255, green: CGFloat((hex >> 8) & 255) / 255,
                  blue: CGFloat(hex & 255) / 255, alpha: 1)
    }
}
