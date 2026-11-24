// SPDX-License-Identifier: Apache-2.0
import AppKit
import CSkylatchCore
import SkylatchLogic

func printError(_ message: String) {
    FileHandle.standardError.write(Data("Skylatch: \(message)\n".utf8))
}

/// Blocking device discovery through the core (call off the main thread).
func discoverDevices(hub: String, token: String) throws -> [Device] {
    var err = [CChar](repeating: 0, count: 1024)
    guard let json = skylatch_list_devices_json(hub, token, &err, err.count) else {
        let text = String(cString: err)
        throw UsageError(text.isEmpty ? "device discovery failed" : text)
    }
    defer { skylatch_free_string(json) }
    return try Devices.parse(json: Data(String(cString: json).utf8))
}

/// Application controller: window, session lifecycle, device picker, smoke mode and status updates.
final class AppController: NSObject, NSApplicationDelegate, NSWindowDelegate, SessionDelegate {
    private var config: Config
    private var window: NSWindow?
    private var view: VideoView?
    private var renderer: MetalRenderer?
    private var session: Session?
    private var clipboard: ClipboardSync?
    private var timer: Timer?
    private var shown = false
    private var finished = false
    private var hud: HudOverlay?
    private let keyCapture = KeyCapture()
    private let text = HudText.current
    private var deviceID = ""
    private var deviceName = ""
    private var swapEnabled = false
    private var captureOn = false
    private var audioOn = true
    private var windowedCapture = false
    private var fullscreen = false
    private var monitors: [MonitorEntry] = []
    private var revealFallback: DispatchWorkItem?
    /// How long a connected session may stay without a picture before the window is shown anyway.
    private static let revealFallbackSeconds = 1.5
    /// Share of the screen's visible area the first-frame window may fill.
    private static let screenFraction = 0.85
    private var connectedAt = DispatchTime.now().uptimeNanoseconds
    private var previousStats = SessionStats()
    private var previousTitleAt = DispatchTime.now().uptimeNanoseconds

    init(config: Config) {
        self.config = config
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        do {
            try prepare()
        } catch {
            fail("\(error)")
            return
        }
        if config.hub.isEmpty {
            // Direct sessions have no device record: label the HUD with the endpoint's host.
            let authority = config.direct.components(separatedBy: "://").last ?? config.direct
            applyDevice(id: "", name: String(authority.prefix { !"/?#".contains($0) }), os: nil)
            connect()
        } else if config.isSmoke && !config.device.isEmpty {
            connect(device: config.device)
        } else {
            // Hub mode looks the device up first: its name and OS drive the HUD label and the Cmd/Ctrl default.
            let hub = config.hub, token = config.token
            DispatchQueue.global(qos: .userInitiated).async {
                let result = Result { try discoverDevices(hub: hub, token: token) }
                DispatchQueue.main.async { [self] in
                    switch result {
                    case .success(let devices): choose(from: devices)
                    case .failure(let error):
                        // With an explicit --device the lookup is only a convenience; the connect reports real errors.
                        if config.device.isEmpty { fail("\(error)") } else { connect(device: config.device) }
                    }
                }
            }
        }
    }

    // MARK: Setup

    private func prepare() throws {
        let layer: CAMetalLayer
        if config.isSmoke {
            // Hidden: a detached Metal layer that follows the video size. No window is ever created.
            layer = CAMetalLayer()
            renderer = try MetalRenderer(layer: layer, followsVideoSize: true)
            return
        }
        let view = VideoView(frame: NSRect(x: 0, y: 0, width: 1100, height: 730))
        layer = view.metalLayer
        let renderer = try MetalRenderer(layer: layer, followsVideoSize: false)
        view.videoSize = { [weak renderer] in renderer?.videoSize ?? (0, 0) }
        view.onResize = { [weak renderer] in renderer?.redraw() }
        view.sendRecord = { [weak self] record in self?.session?.sendInput(record) }
        view.smoothWheel = { [weak self] in self?.session?.smoothWheelEnabled ?? false }
        view.onToggleCapture = { [weak self] in self?.toggleCapture() }
        view.onEscapeHold = { [weak self] in self?.window?.toggleFullScreen(nil) }
        let hud = HudOverlay(frame: view.bounds)
        hud.onAction = { [weak self] action in self?.handleHud(action) }
        view.addSubview(hud)
        view.hud = hud
        self.hud = hud
        keyCapture.handler = { [weak self] code, down, flags, isRepeat in
            guard let self, NSApp.isActive, self.window?.isKeyWindow == true else { return false }
            return self.view?.handleCapturedKey(keyCode: code, down: down, flags: flags, isRepeat: isRepeat) ?? false
        }
        let window = NSWindow(contentRect: view.frame, styleMask: [.titled, .closable, .miniaturizable, .resizable],
                              backing: .buffered, defer: false)
        window.title = "Skylatch | Waiting for video"
        window.contentView = view
        window.contentMinSize = NSSize(width: 320, height: 200)
        window.collectionBehavior = [.fullScreenPrimary]
        window.isReleasedWhenClosed = false
        window.delegate = self
        self.view = view
        self.renderer = renderer
        self.window = window
        installMenu()
    }

    private func installMenu() {
        let main = NSMenu()
        let appItem = NSMenuItem()
        main.addItem(appItem)
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "Quit Skylatch", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu
        let viewItem = NSMenuItem()
        main.addItem(viewItem)
        let viewMenu = NSMenu(title: "View")
        let fullscreen = NSMenuItem(title: "Toggle Full Screen", action: #selector(NSWindow.toggleFullScreen(_:)), keyEquivalent: "f")
        fullscreen.keyEquivalentModifierMask = [.command, .control]
        viewMenu.addItem(fullscreen)
        viewItem.submenu = viewMenu
        NSApp.mainMenu = main
    }

    // MARK: Device choice

    private func choose(from devices: [Device]) {
        guard !devices.isEmpty else { return fail("No devices are online on this hub") }
        if !config.device.isEmpty {
            // Explicit --device: use its record when listed, otherwise let the connect report the problem.
            if let known = devices.first(where: { $0.id == config.device }) {
                applyDevice(id: known.id, name: known.name, os: known.os)
                return connect()
            }
            return connect(device: config.device)
        }
        var index = 0
        if devices.count > 1 {
            guard !config.isSmoke else { return fail("multiple devices are online; specify --device for smoke mode") }
            guard let picked = pickDevice(devices) else { return finish(code: 0) }
            index = picked
        }
        applyDevice(id: devices[index].id, name: devices[index].name, os: devices[index].os)
        connect()
    }

    /// Hub session without a device record (smoke mode or a failed lookup).
    private func connect(device id: String) {
        applyDevice(id: id, name: id, os: nil)
        connect()
    }

    private func applyDevice(id: String, name: String, os: String?) {
        deviceID = id
        deviceName = Devices.displayText(name)
        swapEnabled = ModifierSwap.resolve(flag: config.swapCmdCtrl, deviceOS: os)
        view?.swapCmdCtrl = swapEnabled
        hud?.setDevice(deviceName)
        syncHud()
    }

    /// Native picker: an alert with a pop-up button listing every online device.
    private func pickDevice(_ devices: [Device]) -> Int? {
        activateApp()
        let alert = NSAlert()
        alert.messageText = "Choose a Skylatch device"
        alert.informativeText = "Select a device to connect to:"
        let popup = NSPopUpButton(frame: NSRect(x: 0, y: 0, width: 420, height: 26), pullsDown: false)
        popup.addItems(withTitles: devices.map(\.pickerLabel))
        alert.accessoryView = popup
        alert.addButton(withTitle: "Connect")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else { return nil }
        return popup.indexOfSelectedItem
    }

    // MARK: Connection

    private func connect() {
        guard let renderer else { return }
        let session = Session(config: config, renderer: renderer, delegate: self)
        self.session = session
        audioOn = true
        hud?.setAudioAvailable(session.hasAudio)
        hud?.setAudioEnabled(true)
        session.start(device: deviceID)
    }

    /// Re-establishes the session in the same window; the last picture stays under the card meanwhile.
    private func reconnect() {
        guard !finished, hud?.isLostCardVisible == true else { return }
        hud?.showReconnecting()
        session?.stop()
        session = nil
        previousStats = SessionStats()
        connect()
    }

    private func userDisconnect() {
        guard !finished, session != nil else { return }
        sessionEnded(SessionEnd(message: text.userDisconnected, clean: true))
    }

    func sessionConnected() {
        guard !finished, let session else { return }
        connectedAt = DispatchTime.now().uptimeNanoseconds
        previousTitleAt = connectedAt
        let timer = Timer(timeInterval: config.isSmoke ? 0.05 : 1.0, repeats: true) { [weak self] _ in self?.tick() }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
        if config.isSmoke { return }
        view?.inputEnabled = true
        if !config.noClipboard {
            let sync = ClipboardSync { [weak session] text in session?.sendClipboard(text) ?? false }
            sync.start()
            clipboard = sync
        }
        if !shown {
            // The window stays hidden until the first picture sizes it; show it anyway if none arrives.
            let fallback = DispatchWorkItem { [weak self] in self?.revealWindow(videoSize: nil) }
            revealFallback = fallback
            DispatchQueue.main.asyncAfter(deadline: .now() + Self.revealFallbackSeconds, execute: fallback)
        }
    }

    func sessionFirstFrame(width: Int, height: Int) {
        guard !finished, !config.isSmoke else { return }
        revealWindow(videoSize: (width, height))
        // A reconnect that has a picture again drops its card.
        hud?.dismissReconnecting()
    }

    func sessionMonitors(_ list: [MonitorEntry]) {
        guard !finished else { return }
        monitors = list
        hud?.setMonitors(active: Monitors.activeIndex(list), count: list.count)
    }

    // MARK: Window reveal

    /// The screen the user is working on: the one under the pointer.
    private func activeScreen() -> NSScreen? {
        let mouse = NSEvent.mouseLocation
        return NSScreen.screens.first { NSMouseInRect(mouse, $0.frame, false) } ?? NSScreen.main ?? NSScreen.screens.first
    }

    /// Shows the window once: sized to the stream's exact aspect ratio inside 85% of the active screen's
    /// visible area (so there are no black bars), and centered there.
    private func revealWindow(videoSize: (width: Int, height: Int)?) {
        guard !shown, !finished, let window else { return }
        shown = true
        revealFallback?.cancel()
        revealFallback = nil
        let screen = activeScreen()
        let visible = screen?.visibleFrame ?? .zero
        if let videoSize, visible.width > 0,
           let fit = Geometry.aspectFit(videoWidth: Double(videoSize.width), videoHeight: Double(videoSize.height),
                                        maxWidth: Double(visible.width) * Self.screenFraction,
                                        maxHeight: Double(visible.height) * Self.screenFraction,
                                        minWidth: Double(window.contentMinSize.width),
                                        minHeight: Double(window.contentMinSize.height)) {
            window.setContentSize(NSSize(width: fit.width, height: fit.height))
        }
        if visible.width > 0 {
            let frame = window.frame
            let x = min(max(visible.midX - frame.width / 2, visible.minX), max(visible.minX, visible.maxX - frame.width))
            let y = min(max(visible.midY - frame.height / 2, visible.minY), max(visible.minY, visible.maxY - frame.height))
            window.setFrameOrigin(NSPoint(x: x.rounded(), y: y.rounded()))
        } else {
            window.center()
        }
        window.makeKeyAndOrderFront(nil)
        activateApp()
        if let view { window.makeFirstResponder(view) }
    }

    // MARK: HUD actions and keyboard capture

    private func syncHud() {
        hud?.setFlags(capture: captureOn, swap: swapEnabled, fullscreen: fullscreen)
    }

    private func handleHud(_ action: HudOverlay.Action) {
        switch action {
        case .monitor:
            if let next = Monitors.next(after: monitors) { session?.selectMonitor(next.id) }
        case .audio: toggleAudio()
        case .capture: toggleCapture()
        case .swap: toggleSwap()
        case .fullscreen: window?.toggleFullScreen(nil)
        case .disconnect: userDisconnect()
        case .reconnect: reconnect()
        case .close: window?.close()
        case .none, .surface: break
        }
    }

    private func toggleAudio() {
        guard let session, session.hasAudio, session.setAudio(!audioOn) else { return }
        audioOn.toggle()
        hud?.setAudioEnabled(audioOn)
        hud?.toast(audioOn ? text.audioOn : text.audioOff)
    }

    /// Turns system-key capture on or off. Turning it on needs the Accessibility permission; without it the
    /// request is refused and the toast says why.
    private func setCapture(_ on: Bool) {
        guard on != captureOn else { return }
        view?.releaseAll()
        if on {
            guard keyCapture.start() else {
                hud?.toast(text.captureNeedsAccess)
                return
            }
        } else {
            keyCapture.stop()
        }
        captureOn = on
        syncHud()
    }

    private func toggleCapture() {
        guard !finished else { return }
        let target = !captureOn
        setCapture(target)
        guard captureOn == target else { return }
        // In fullscreen the capture follows the fullscreen state; only the windowed choice is remembered.
        if !fullscreen { windowedCapture = captureOn }
        hud?.toast(captureOn ? text.captureOn : text.captureOff)
    }

    private func toggleSwap() {
        swapEnabled.toggle()
        view?.releaseAll()
        view?.swapCmdCtrl = swapEnabled
        syncHud()
        hud?.toast(swapEnabled ? text.swapOn : text.swapOff)
    }

    func sessionCursor(width: Int, height: Int, xhot: Int, yhot: Int, rgba: [UInt8]?) {
        guard !finished, let view else { return }
        view.setRemoteCursor(width: width, height: height, xhot: xhot, yhot: yhot, rgba: rgba)
    }

    func sessionClipboardText(_ text: String) {
        guard !finished else { return }
        clipboard?.apply(text)
    }

    func sessionEnded(_ end: SessionEnd) {
        guard !finished else { return }
        timer?.invalidate()
        clipboard?.stop()
        view?.inputEnabled = false
        view?.releaseAll()
        session?.stop()
        if !config.isSmoke && shown {
            // Keep the window; the card over the last picture says why and offers to reconnect.
            view?.setRemoteCursor(width: 0, height: 0, xhot: 0, yhot: 0, rgba: nil)
            window?.title = "Skylatch | Disconnected: \(Devices.displayText(end.message))"
            hud?.showLost(Devices.displayText(end.message))
            return
        }
        if config.isSmoke {
            printError(end.message)
            finish(code: 1)
        } else {
            fail(end.message)
        }
    }

    // MARK: Status timer and smoke exit

    private func tick() {
        guard let session, !finished else { return }
        let stats = session.snapshot()
        let now = DispatchTime.now().uptimeNanoseconds
        if config.isSmoke {
            if stats.presented > 0 && now &- stats.firstPresent >= UInt64(config.smokeSeconds) * 1_000_000_000 {
                finishSmoke(stats)
                return
            }
        } else {
            let seconds = max(0.001, Double(now &- previousTitleAt) / 1e9)
            let fps = Double(stats.presented &- previousStats.presented) / seconds
            window?.title = String(format: "Skylatch | %dx%d | %.1f FPS | Cmd+Ctrl+F: Fullscreen", stats.width, stats.height, fps)
            let presented = stats.presented &- previousStats.presented
            let latencyMs = presented == 0 ? 0 : Double(stats.waitUs &- previousStats.waitUs) / Double(presented) / 1000
            hud?.setStat(HudFormat.stats(width: stats.width, height: stats.height, fps: fps, latencyMs: latencyMs))
            previousStats = stats
            previousTitleAt = now
        }
        let sinceConnect = now &- connectedAt
        if stats.decoded == 0 && sinceConnect >= 8_000_000_000 {
            session.report("timed out waiting for H.264 video")
        } else if config.isSmoke && stats.presented == 0 && sinceConnect >= 8_000_000_000 {
            session.report("the hidden Metal layer did not present any frames")
        }
    }

    private func finishSmoke(_ stats: SessionStats) {
        timer?.invalidate()
        session?.stop()
        let avgDecodeMs = Double(stats.decodeUs) / (1000.0 * Double(max(stats.decoded, 1)))
        let avgWaitUs = Double(stats.waitUs) / Double(max(stats.presented, 1))
        print(String(format: "resolution=%dx%d frames_decoded=%llu frames_presented=%llu avg_decode_ms=%.3f avg_wait_us=%.1f decode_mode=%@",
                     stats.width, stats.height, stats.decoded, stats.presented, avgDecodeMs, avgWaitUs, stats.mode))
        finish(code: 0)
    }

    // MARK: Window delegate and shutdown

    func windowDidResignKey(_ notification: Notification) { view?.releaseAll() }

    func windowWillEnterFullScreen(_ notification: Notification) { windowedCapture = captureOn }

    /// Fullscreen captures system keys automatically.
    func windowDidEnterFullScreen(_ notification: Notification) {
        fullscreen = true
        setCapture(true)
        syncHud()
        if captureOn { hud?.toast(text.fullscreenOn) }
    }

    /// Leaving fullscreen restores the windowed capture preference (off unless the user turned it on).
    func windowDidExitFullScreen(_ notification: Notification) {
        fullscreen = false
        setCapture(windowedCapture)
        syncHud()
        hud?.toast(text.fullscreenOff)
    }

    func windowWillClose(_ notification: Notification) {
        view?.releaseAll()
        finish(code: 0)
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        view?.releaseAll()
        return .terminateNow
    }

    func applicationWillTerminate(_ notification: Notification) {
        teardown()
    }

    private func teardown() {
        finished = true
        timer?.invalidate()
        revealFallback?.cancel()
        keyCapture.stop()
        clipboard?.stop()
        view?.inputEnabled = false
        session?.stop()
        session = nil
    }

    private func activateApp() {
        // The cooperative activate() on macOS 14+ is refused when another app is frontmost, which is the
        // normal case for a viewer started from a terminal; the explicit form still brings the app forward.
        NSApp.activate(ignoringOtherApps: true)
    }

    private func fail(_ message: String) {
        if config.isSmoke || !NSApp.isRunning {
            printError(message)
        } else {
            printError(message)
            activateApp()
            let alert = NSAlert()
            alert.alertStyle = .critical
            alert.messageText = "Skylatch"
            alert.informativeText = Devices.displayText(message)
            alert.addButton(withTitle: "OK")
            alert.runModal()
        }
        finish(code: 1)
    }

    private func finish(code: Int32) {
        if !finished { teardown() }
        exit(code)
    }
}
