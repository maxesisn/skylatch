// SPDX-License-Identifier: Apache-2.0
import Foundation

/// One display from an SL1 Type 17 monitor list.
public struct MonitorEntry: Equatable {
    public var id: String
    public var name: String
    public var active: Bool

    public init(id: String, name: String, active: Bool) {
        self.id = id; self.name = name; self.active = active
    }
}

public enum Monitors {
    private struct Raw: Decodable {
        var id: String
        var name: String
        var active: Bool?
    }

    /// Parses the JSON array handed to the monitor-list callback. Nil when it is malformed.
    public static func parse(json: Data) -> [MonitorEntry]? {
        guard let raw = try? JSONDecoder().decode([Raw].self, from: json) else { return nil }
        let list = raw.map { MonitorEntry(id: $0.id, name: Devices.displayText($0.name), active: $0.active ?? false) }
        guard list.allSatisfy({ !$0.id.isEmpty && !$0.id.contains("\0") && $0.id.utf8.count <= 256 }) else { return nil }
        return list
    }

    /// Index of the active display (0 when none is flagged).
    public static func activeIndex(_ list: [MonitorEntry]) -> Int {
        list.firstIndex(where: \.active) ?? 0
    }

    /// The display that follows the active one, wrapping around; nil with fewer than two displays.
    public static func next(after list: [MonitorEntry]) -> MonitorEntry? {
        guard list.count > 1 else { return nil }
        return list[(activeIndex(list) + 1) % list.count]
    }
}

public enum HudFormat {
    /// Live stats line, three groups set apart by spacing: `1920x1080   60 FPS   3.4 ms`.
    public static func stats(width: Int, height: Int, fps: Double, latencyMs: Double) -> String {
        guard width > 0, height > 0 else { return "" }
        return String(format: "%dx%d\u{2003}%.0f FPS\u{2003}%.1f ms", width, height, max(0, fps), max(0, latencyMs))
    }
}
