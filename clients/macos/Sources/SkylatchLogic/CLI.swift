// SPDX-License-Identifier: Apache-2.0
import Foundation

public struct Config: Equatable {
    public var hub = ""
    public var direct = ""
    public var token = ""
    public var device = ""
    public var smokeSeconds: UInt = 0
    public var list = false
    public var noDirect = false
    public var noClipboard = false
    public var help = false

    public init() {}

    public var isSmoke: Bool { smokeSeconds > 0 }
}

public struct UsageError: Error, Equatable, CustomStringConvertible {
    public let message: String
    public init(_ message: String) { self.message = message }
    public var description: String { message }
}

public enum CLI {
    public static let usage = """
        Usage:
          skylatch-client --hub <ws:// or wss:// URL> --token <token> --list
          skylatch-client --hub <ws:// or wss:// URL> --token <token> [--device <id>]
                          [--no-direct] [--no-clipboard] [--smoke-seconds N]
          skylatch-client --direct <ws:// or wss:// URL> [--token <64-hex>]
                          [--no-clipboard] [--smoke-seconds N]
          skylatch-client --help

        Without --device, hub mode selects the sole device or opens a picker.
        --no-direct forces the hub relay. --no-clipboard disables text clipboard sync.
        --smoke-seconds runs hidden without input or clipboard sync;
        specify --device if the hub has multiple devices.
        Cmd+Ctrl+F: toggle native fullscreen.
        """

    /// Accepts ws:// or wss:// URLs with a host and no user information or whitespace.
    public static func isWebSocketURL(_ url: String) -> Bool {
        var rest = Substring(url)
        if rest.hasPrefix("ws://") { rest = rest.dropFirst(5) }
        else if rest.hasPrefix("wss://") { rest = rest.dropFirst(6) }
        else { return false }
        let host = rest.prefix { !"/?#".contains($0) }
        return !host.isEmpty && !host.contains("@") && !url.contains { " \t\r\n".contains($0) }
    }

    static func isHex64(_ s: String) -> Bool {
        s.utf8.count == 64 && s.utf8.allSatisfy { ($0 >= 48 && $0 <= 57) || ($0 >= 65 && $0 <= 70) || ($0 >= 97 && $0 <= 102) }
    }

    /// Parses arguments (excluding the program name). Throws `UsageError` for anything invalid.
    public static func parse(_ args: [String]) throws -> Config {
        var c = Config()
        if args.count == 1 && (args[0] == "--help" || args[0] == "-h") {
            c.help = true
            return c
        }
        var smokeSeen = false
        var i = 0
        while i < args.count {
            let option = args[i]
            switch option {
            case "--list":
                if c.list { throw UsageError("duplicate option; see --help") }
                c.list = true
            case "--no-direct":
                if c.noDirect { throw UsageError("duplicate option; see --help") }
                c.noDirect = true
            case "--no-clipboard":
                if c.noClipboard { throw UsageError("duplicate option; see --help") }
                c.noClipboard = true
            case "--hub", "--direct", "--token", "--device", "--smoke-seconds":
                guard i + 1 < args.count, !args[i + 1].hasPrefix("--") else {
                    throw UsageError("missing option value; see --help")
                }
                i += 1
                let value = args[i]
                if value.isEmpty { throw UsageError("empty option value; see --help") }
                func assign(_ keyPath: WritableKeyPath<Config, String>) throws {
                    if !c[keyPath: keyPath].isEmpty { throw UsageError("duplicate option; see --help") }
                    c[keyPath: keyPath] = value
                }
                switch option {
                case "--hub": try assign(\.hub)
                case "--direct": try assign(\.direct)
                case "--token": try assign(\.token)
                case "--device": try assign(\.device)
                default:
                    if smokeSeen { throw UsageError("duplicate --smoke-seconds") }
                    smokeSeen = true
                    guard let n = UInt(value), n >= 1, n <= 86400, value.utf8.allSatisfy({ $0 >= 48 && $0 <= 57 }) else {
                        throw UsageError("--smoke-seconds must be 1..86400")
                    }
                    c.smokeSeconds = n
                }
            default:
                throw UsageError("unknown option; see --help")
            }
            i += 1
        }
        if c.hub.isEmpty == c.direct.isEmpty {
            throw UsageError("specify exactly one of --hub or --direct; see --help")
        }
        guard isWebSocketURL(c.hub.isEmpty ? c.direct : c.hub) else {
            throw UsageError("endpoint must be a ws:// or wss:// URL without user information")
        }
        if !c.hub.isEmpty {
            if c.token.isEmpty { throw UsageError("--hub requires --token") }
            if c.list && (!c.device.isEmpty || c.noDirect || c.noClipboard || c.isSmoke) {
                throw UsageError("--list cannot be used with session options")
            }
        } else {
            if c.list || !c.device.isEmpty || c.noDirect {
                throw UsageError("--list, --device and --no-direct require --hub")
            }
            if !c.token.isEmpty && !isHex64(c.token) {
                throw UsageError("direct --token must be 64 hexadecimal digits")
            }
        }
        return c
    }
}

public struct Device: Equatable {
    public var id: String
    public var name: String
    public var os: String
    public var monitors: Int
    public var endpoints: String

    public init(id: String, name: String, os: String, monitors: Int, endpoints: String) {
        self.id = id; self.name = name; self.os = os; self.monitors = monitors; self.endpoints = endpoints
    }

    /// Picker label: `name  [id]  os`.
    public var pickerLabel: String { "\(name)  [\(id)]  \(os)" }
}

public enum Devices {
    /// Replaces control characters so remote metadata cannot inject terminal escapes or extra rows.
    public static func displayText(_ value: String) -> String {
        String(String.UnicodeScalarView(value.unicodeScalars.map { s in
            (s.value < 32 || (s.value >= 127 && s.value <= 159) || s.value == 0x2028 || s.value == 0x2029) ? " " : s
        }))
    }

    private struct Endpoint: Decodable { var kind: String; var host: String; var port: Int }
    private struct Raw: Decodable {
        var device_id: String
        var name: String
        var os: String
        var monitors: Int
        var direct_endpoints: [Endpoint]
    }

    /// Parses the JSON array returned by `skylatch_list_devices_json`.
    public static func parse(json: Data) throws -> [Device] {
        let raw: [Raw]
        do { raw = try JSONDecoder().decode([Raw].self, from: json) }
        catch { throw UsageError("could not read the hub device list") }
        return try raw.map { r in
            guard !r.device_id.isEmpty, !r.device_id.contains("\0") else {
                throw UsageError("hub returned an invalid device ID")
            }
            let endpoints = r.direct_endpoints.map { e -> String in
                var host = displayText(e.host)
                if host.contains(":") && !host.hasPrefix("[") { host = "[\(host)]" }
                return "\(displayText(e.kind)):\(host):\(e.port)"
            }
            return Device(id: r.device_id, name: displayText(r.name), os: displayText(r.os), monitors: r.monitors,
                          endpoints: endpoints.isEmpty ? "-" : endpoints.joined(separator: ", "))
        }
    }

    /// Column-aligned table: device_id, name, os, monitors, direct_endpoints.
    public static func table(_ devices: [Device]) -> String {
        var rows: [[String]] = [["device_id", "name", "os", "monitors", "direct_endpoints"]]
        for d in devices { rows.append([displayText(d.id), d.name, d.os, String(d.monitors), d.endpoints]) }
        var widths = [Int](repeating: 0, count: 4)
        for row in rows { for i in 0..<4 { widths[i] = max(widths[i], row[i].count) } }
        return rows.map { row in
            var line = ""
            for i in 0..<4 { line += row[i] + String(repeating: " ", count: widths[i] - row[i].count + 2) }
            return line + row[4]
        }.joined(separator: "\n")
    }
}
