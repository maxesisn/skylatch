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
        if config.hub.isEmpty || !config.device.isEmpty {
            connect(device: config.device)
        } else {
            let hub = config.hub, token = config.token
            DispatchQueue.global(qos: .userInitiated).async {
                let result = Result { try discoverDevices(hub: hub, token: token) }
                DispatchQueue.main.async { [self] in
                    switch result {
                    case .success(let devices): choose(from: devices)
                    case .failure(let error): fail("\(error)")
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
        let window = NSWindow(contentRect: view.frame, styleMask: [.titled, .closable, .miniaturizable, .resizable],
                              backing: .buffered, defer: false)
        window.title = "Skylatch | Waiting for video"
        window.contentView = view
        window.contentMinSize = NSSize(width: 320, height: 200)
        window.collectionBehavior = [.fullScreenPrimary]
        window.isReleasedWhenClosed = false
        window.delegate = self
        window.center()
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
        var index = 0
        if devices.count > 1 {
            guard !config.isSmoke else { return fail("multiple devices are online; specify --device for smoke mode") }
            guard let picked = pickDevice(devices) else { return finish(code: 0) }
            index = picked
        }
        connect(device: devices[index].id)
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

    private func connect(device: String) {
        guard let renderer else { return }
        let session = Session(config: config, renderer: renderer, delegate: self)
        self.session = session
        session.start(device: device)
    }

    func sessionConnected() {
        guard !finished, let session else { return }
        connectedAt = DispatchTime.now().uptimeNanoseconds
        previousTitleAt = connectedAt
        let timer = Timer(timeInterval: config.isSmoke ? 0.05 : 1.0, repeats: true) { [weak self] _ in self?.tick() }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
        if config.isSmoke { return }
        if !config.noClipboard {
            let sync = ClipboardSync { [weak session] text in session?.sendClipboard(text) ?? false }
            sync.start()
            clipboard = sync
        }
        shown = true
        window?.makeKeyAndOrderFront(nil)
        activateApp()
        if let view { window?.makeFirstResponder(view) }
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
            // Keep the window so the reason stays visible; the last picture remains on screen.
            view?.setRemoteCursor(width: 0, height: 0, xhot: 0, yhot: 0, rgba: nil)
            window?.title = "Skylatch | Disconnected: \(Devices.displayText(end.message))"
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
