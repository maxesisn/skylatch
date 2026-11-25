// SPDX-License-Identifier: Apache-2.0
import Foundation

/// Visibility of the performance HUD; the shortcut and the latch button step through the cases in order.
public enum PerfHudMode: Int, CaseIterable {
    case hidden, card, strip

    public var next: PerfHudMode {
        switch self {
        case .hidden: return .card
        case .card: return .strip
        case .strip: return .hidden
        }
    }
}

/// Averages over the last second of a session.
public struct PerfRates: Equatable {
    public var fps: Double
    public var mbps: Double
    /// Nil when nothing was decoded, presented or queued for playback in the window.
    public var decodeMs: Double?
    public var waitMs: Double?
    public var audioMs: Double?

    public init(fps: Double = 0, mbps: Double = 0, decodeMs: Double? = nil, waitMs: Double? = nil, audioMs: Double? = nil) {
        self.fps = fps; self.mbps = mbps; self.decodeMs = decodeMs; self.waitMs = waitMs; self.audioMs = audioMs
    }
}

/// Sliding window of per-frame and per-audio-packet measurements. Timestamps are uptime nanoseconds;
/// a sample counts while it is newer than `span` before the time asked about.
public struct PerfWindow {
    public let span: UInt64
    private var received: [(at: UInt64, bytes: Int)] = []
    private var decoded: [(at: UInt64, us: UInt64)] = []
    private var presented: [(at: UInt64, waitUs: UInt64)] = []
    private var audio: [(at: UInt64, ms: Double)] = []

    public init(span: UInt64 = 1_000_000_000) {
        precondition(span > 0)
        self.span = span
    }

    /// An H.264 access unit of `bytes` arrived from the network.
    public mutating func received(bytes: Int, at: UInt64) {
        received.append((at, bytes))
        prune(at)
    }

    /// A frame finished decoding; `waitUs` (arrival to present) is counted only when it reached the screen.
    public mutating func decoded(decodeUs: UInt64, waitUs: UInt64, presented shown: Bool, at: UInt64) {
        decoded.append((at, decodeUs))
        if shown { presented.append((at, waitUs)) }
        prune(at)
    }

    /// Playback delay the newest audio packet joined, in milliseconds.
    public mutating func audioBuffered(ms: Double, at: UInt64) {
        audio.append((at, ms))
        prune(at)
    }

    public mutating func reset() {
        received.removeAll(); decoded.removeAll(); presented.removeAll(); audio.removeAll()
    }

    public mutating func rates(at now: UInt64) -> PerfRates {
        prune(now)
        let seconds = Double(span) / 1e9
        func mean<T>(_ list: [T], _ value: (T) -> Double) -> Double? {
            list.isEmpty ? nil : list.reduce(0) { $0 + value($1) } / Double(list.count)
        }
        return PerfRates(
            fps: Double(presented.count) / seconds,
            mbps: Double(received.reduce(0) { $0 + $1.bytes }) * 8 / 1e6 / seconds,
            decodeMs: mean(decoded) { Double($0.us) / 1000 },
            waitMs: mean(presented) { Double($0.waitUs) / 1000 },
            audioMs: mean(audio) { $0.ms })
    }

    private mutating func prune(_ now: UInt64) {
        guard now > span else { return }
        let cutoff = now - span
        received.removeFirst(received.prefix { $0.at <= cutoff }.count)
        decoded.removeFirst(decoded.prefix { $0.at <= cutoff }.count)
        presented.removeFirst(presented.prefix { $0.at <= cutoff }.count)
        audio.removeFirst(audio.prefix { $0.at <= cutoff }.count)
    }
}

/// How the stream reaches this viewer. `unknown` before a connection is established.
public enum PerfPath { case direct, relay, unknown }

public enum PerfAudio: Equatable { case off, muted, on }

/// Everything one HUD refresh shows.
public struct PerfReadout: Equatable {
    public var width: Int
    public var height: Int
    public var decoder: String
    public var path: PerfPath
    public var audio: PerfAudio
    public var rates: PerfRates

    public init(width: Int = 0, height: Int = 0, decoder: String = "", path: PerfPath = .unknown,
                audio: PerfAudio = .off, rates: PerfRates = PerfRates()) {
        self.width = width; self.height = height; self.decoder = decoder; self.path = path
        self.audio = audio; self.rates = rates
    }

    /// Below 30 FPS the status dot turns amber.
    public var isSlow: Bool { rates.fps < 30 }
}

public struct PerfHudLabels: Equatable {
    public var decode, wait, bitrate, audio, direct, relay, muted, off, idle: String

    public static let zh = PerfHudLabels(decode: "解", wait: "等", bitrate: "码率", audio: "音", direct: "直连",
                                         relay: "中继", muted: "已静音", off: "关闭", idle: "无数据")
    public static let en = PerfHudLabels(decode: "Dec", wait: "Wait", bitrate: "Rate", audio: "Audio", direct: "Direct",
                                         relay: "Relay", muted: "Muted", off: "Off", idle: "No data")
}

/// One card row: text on the left edge and text on the right edge.
public struct PerfRow: Equatable {
    public var left: String
    public var right: String
}

public enum PerfHudFormat {
    private static let gap = "\u{2003}"

    static func ms(_ value: Double?) -> String {
        guard let value else { return "--" }
        return String(format: "%.1fms", max(0, value))
    }

    static func mbps(_ value: Double) -> String {
        String(format: value >= 100 ? "%.0f Mbps" : "%.1f Mbps", max(0, value))
    }

    static func fps(_ value: Double) -> String { String(format: "%.0f FPS", max(0, value)) }

    static func resolution(_ r: PerfReadout) -> String {
        r.width > 0 && r.height > 0 ? "\(r.width)\u{00D7}\(r.height)" : ""
    }

    static func path(_ r: PerfReadout, _ l: PerfHudLabels) -> String {
        switch r.path {
        case .direct: return l.direct
        case .relay: return l.relay
        case .unknown: return ""
        }
    }

    static func decoder(_ r: PerfReadout) -> String {
        [r.decoder, ms(r.rates.decodeMs)].filter { !$0.isEmpty }.joined(separator: " ")
    }

    static func audio(_ r: PerfReadout, _ l: PerfHudLabels) -> String {
        switch r.audio {
        case .off: return l.off
        case .muted: return l.muted
        case .on: return r.rates.audioMs.map { String(format: "Opus %.0fms", max(0, $0)) } ?? l.idle
        }
    }

    private static func join(_ parts: [String]) -> String { parts.filter { !$0.isEmpty }.joined(separator: gap) }

    /// The three rows of the card. The status dot sits before the first row's left text.
    public static func card(_ r: PerfReadout, labels l: PerfHudLabels) -> [PerfRow] {
        [
            PerfRow(left: join([fps(r.rates.fps), resolution(r)]), right: path(r, l)),
            PerfRow(left: "\(l.decode) \(decoder(r))", right: "\(l.wait) \(ms(r.rates.waitMs))"),
            PerfRow(left: "\(l.bitrate) \(mbps(r.rates.mbps))", right: "\(l.audio) \(audio(r, l))"),
        ]
    }

    /// The single-line strip, after the status dot.
    public static func strip(_ r: PerfReadout, labels l: PerfHudLabels) -> String {
        join([fps(r.rates.fps), resolution(r), decoder(r), mbps(r.rates.mbps), path(r, l)])
    }
}

/// Where the HUD may sit: wholly inside the view, measured from its top-left corner.
public enum PerfHudPlacement {
    public static let defaultOrigin = (x: 14.0, y: 14.0)

    public static func clamp(x: Double, y: Double, width: Double, height: Double,
                             boundsWidth: Double, boundsHeight: Double) -> (x: Double, y: Double) {
        (min(max(x, 0), max(0, boundsWidth - width)), min(max(y, 0), max(0, boundsHeight - height)))
    }
}

/// A drag that keeps the grabbed point of the HUD under the pointer.
public struct PerfHudDrag {
    public let grabX: Double
    public let grabY: Double

    public init(pointerX: Double, pointerY: Double, originX: Double, originY: Double) {
        grabX = pointerX - originX
        grabY = pointerY - originY
    }

    public func origin(pointerX: Double, pointerY: Double, width: Double, height: Double,
                       boundsWidth: Double, boundsHeight: Double) -> (x: Double, y: Double) {
        PerfHudPlacement.clamp(x: pointerX - grabX, y: pointerY - grabY, width: width, height: height,
                               boundsWidth: boundsWidth, boundsHeight: boundsHeight)
    }
}
