// SPDX-License-Identifier: Apache-2.0
import AppKit
import Foundation
import SkylatchLogic

setvbuf(stdout, nil, _IOLBF, 0)

let config: Config
do {
    config = try CLI.parse(Array(CommandLine.arguments.dropFirst()))
} catch {
    printError("\(error)")
    exit(2)
}

// --help and --list stay console-only: NSApplication is never created for them.
if config.help {
    print(CLI.usage)
    exit(0)
}
if config.list {
    do {
        print(Devices.table(try discoverDevices(hub: config.hub, token: config.token)))
        exit(0)
    } catch {
        printError("\(error)")
        exit(1)
    }
}

let app = NSApplication.shared
// Smoke mode never shows a window, a Dock icon, or activates the app.
app.setActivationPolicy(config.isSmoke ? .prohibited : .regular)
if !config.isSmoke { app.applicationIconImage = NSImage(named: NSImage.computerName) }
let controller = AppController(config: config)
app.delegate = controller
app.run()
