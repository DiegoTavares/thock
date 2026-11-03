import AppIntents
import SwiftUI
import ThockKit
import WidgetKit

@main
struct ThockWatchWidgetBundle: WidgetBundle {
    var body: some Widget {
        TodayComplication()
        CaptureComplication()
        if #available(watchOS 26, *) {
            CaptureControl()
        }
    }
}

// MARK: Today (V48 §7.1)

struct TodayEntry: TimelineEntry {
    enum Content {
        /// No snapshot yet, the phone is not connected, or the snapshot is too
        /// old to hold today: the phone has not been opened in over a week.
        case notConnected
        case day(rows: [WatchRow], moment: WatchTimelineEntry)
    }

    var date: Date
    var content: Content
}

struct TodayProvider: TimelineProvider {
    /// A fixed morning, built without reading a file or making a timeline.
    func placeholder(in context: Context) -> TodayEntry {
        let rows = [
            WatchRow(hash: "1", ordinal: 0, kind: .task, start: 9 * 60, end: 10 * 60, label: "Write the brief"),
            WatchRow(hash: "2", ordinal: 0, kind: .calendar, start: 11 * 60, end: 12 * 60, label: "Standup"),
            WatchRow(hash: "3", ordinal: 0, kind: .reminder, start: 12 * 60, label: "Dentist"),
        ]
        let moment = WatchTimelineEntry(date: Date(), minute: 8 * 60 + 40, windowStart: 8 * 60 + 30, next: rows[0], following: rows[1], left: 2)
        return TodayEntry(date: moment.date, content: .day(rows: rows, moment: moment))
    }

    func getSnapshot(in context: Context, completion: @escaping (TodayEntry) -> Void) {
        completion(entries(now: Date()).first ?? placeholder(in: context))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<TodayEntry>) -> Void) {
        let now = Date()
        let entries = entries(now: now)
        // The snapshot already holds tomorrow, so the new day draws without
        // the phone; the watch app also reloads this when a snapshot arrives.
        let moments = entries.compactMap { entry -> WatchTimelineEntry? in
            if case .day(_, let moment) = entry.content { return moment }
            return nil
        }
        let reload = TimelineEntries.reloadDate(after: moments, today: VaultDay(now)) ?? now.addingTimeInterval(3600)
        completion(Timeline(entries: entries, policy: .after(reload)))
    }

    /// One entry every quarter hour for the next four hours (§7.3).
    func entries(now: Date) -> [TodayEntry] {
        let snapshot = WatchFiles(folder: WatchEnvironment.container).snapshot()
        // The watch's date: a snapshot from before midnight still holds it.
        let today = VaultDay(now)
        guard let snapshot, snapshot.state != .notConnected, snapshot.covers(today) else {
            return [TodayEntry(date: now, content: .notConnected)]
        }
        let rows = snapshot.day(today)?.rows ?? []
        return TimelineEntries.entries(rows: rows, today: today, now: now).map { moment in
            TodayEntry(date: moment.date, content: .day(rows: rows, moment: moment))
        }
    }
}

struct TodayComplication: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WatchEnvironment.todayKind, provider: TodayProvider()) { entry in
            TodayView(entry: entry)
                .containerBackground(for: .widget) { Color.clear }
                .widgetURL(WatchEnvironment.todayURL)
        }
        .configurationDisplayName("Today")
        .description("What comes next, and four hours of today's plan.")
        .supportedFamilies([.accessoryRectangular, .accessoryInline])
    }
}

struct TodayView: View {
    @Environment(\.widgetFamily) private var family
    var entry: TodayEntry

    var body: some View {
        switch entry.content {
        case .notConnected:
            Text("Open Thock on your iPhone")
                .font(.footnote)
        case .day(let rows, let moment):
            if family == .accessoryInline {
                inline(moment)
            } else {
                rectangular(rows: rows, moment: moment)
            }
        }
    }

    private static func clock(_ minutes: Int) -> String {
        ClockTime(clampingHour: minutes / 60 % 24, minute: minutes % 60).description
    }

    private static func next(_ row: WatchRow) -> String {
        "\(row.start.map(clock) ?? "") \(row.label)"
    }

    private func inline(_ moment: WatchTimelineEntry) -> some View {
        Group {
            if let next = moment.next {
                Text("\(moment.left) left · \(Self.next(next))")
            } else if moment.left > 0 {
                Text("\(moment.left) left")
            } else {
                Text("Nothing planned today")
            }
        }
        .privacySensitive()
    }

    private func rectangular(rows: [WatchRow], moment: WatchTimelineEntry) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            if rows.isEmpty {
                Text("Nothing planned today")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(.secondary)
                    .frame(height: NextLine.primaryHeight)
            } else {
                if let next = moment.next {
                    NextLine(row: next, primary: true)
                } else {
                    Text("All done")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(.secondary)
                        .frame(height: NextLine.primaryHeight)
                }
                if let following = moment.following {
                    NextLine(row: following, primary: false)
                } else {
                    Color.clear
                        .frame(height: NextLine.secondaryHeight)
                }
                TimelineTrack(rows: rows, moment: moment)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// One coming item: its kind as a shape, its time, its words. The first is
/// semibold with its time in the accent, the second muted.
struct NextLine: View {
    static let primaryHeight: CGFloat = 19
    static let secondaryHeight: CGFloat = 16

    var row: WatchRow
    var primary: Bool

    private var time: String? {
        row.start.map { ClockTime(clampingHour: $0 / 60 % 24, minute: $0 % 60).description }
    }

    var body: some View {
        let color = primary ? Color.primary : Color.secondary
        HStack(spacing: 4) {
            KindGlyph(kind: row.kind, color: color, height: primary ? 11 : 9)
            if let time {
                if primary {
                    Text(time)
                        .fontDesign(.monospaced)
                        .foregroundStyle(WatchTheme.amber)
                        .widgetAccentable()
                } else {
                    Text(time)
                        .fontDesign(.monospaced)
                }
            }
            Text(row.label)
                .lineLimit(1)
                .privacySensitive()
        }
        .font(.system(size: primary ? 15 : 13, weight: primary ? .semibold : .regular))
        .foregroundStyle(color)
        .frame(height: primary ? Self.primaryHeight : Self.secondaryHeight)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel([kindName, time, row.label].compactMap { $0 }.joined(separator: " "))
    }

    private var kindName: String {
        switch row.kind {
        case .reminder: return "Reminder"
        case .task: return "Task"
        case .calendar: return "Event"
        }
    }
}

/// A row's kind drawn as the track draws it: a filled bar for a task, an
/// outlined one for an event, a bell for a reminder. Told apart by shape on
/// a tinted face. Never an SF Symbol: with `Image(systemName:)` in its
/// entries the complication drew only its redacted placeholder on a real
/// watch (builds 19 and 20), though it rendered in the simulator.
struct KindGlyph: View {
    var kind: WatchRow.Kind
    var color: Color
    var height: CGFloat

    var body: some View {
        Group {
            switch kind {
            case .task:
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(color)
                    .frame(width: 4, height: height)
            case .calendar:
                RoundedRectangle(cornerRadius: 1.5)
                    .strokeBorder(color, lineWidth: 1.2)
                    .frame(width: 5, height: height)
            case .reminder:
                VStack(spacing: 0) {
                    UnevenRoundedRectangle(topLeadingRadius: 3.5, topTrailingRadius: 3.5)
                        .fill(color)
                        .frame(width: 7, height: height - 4)
                    Rectangle()
                        .fill(color)
                        .frame(width: 9, height: 1.5)
                    Circle()
                        .fill(color)
                        .frame(width: 2.5, height: 2.5)
                }
            }
        }
        .frame(width: 9)
    }
}

/// Four hours from the half hour before now: tasks filled, events
/// outlined, reminders a tick and a line, done items faint, now an accent
/// line. Every difference is a shape or an opacity too, for tinted faces.
///
/// Two canvases, not a view per block: the face's shared renderer holds
/// every third-party complication at once and is killed at its memory mark
/// on a busy day, so what it keeps for this one is kept small.
struct TimelineTrack: View {
    var rows: [WatchRow]
    var moment: WatchTimelineEntry

    private let trackHeight: CGFloat = 6
    static let height: CGFloat = 24

    var body: some View {
        ZStack(alignment: .topLeading) {
            Canvas { context, size in
                var baseline = Path()
                baseline.addRect(CGRect(x: 0, y: 3 + trackHeight / 2, width: size.width, height: 1))
                context.fill(baseline, with: .style(.secondary.opacity(0.3)))
                for row in rows {
                    if let begin = row.start {
                        draw(row, from: x(begin, in: size.width), to: x(row.end ?? begin + 30, in: size.width), in: &context)
                    }
                }
                for hour in hours {
                    let label = Text(String(format: "%02d", hour % 24))
                        .font(.system(size: 9, weight: .medium, design: .monospaced))
                        .foregroundStyle(.secondary)
                    // Kept inside the frame, so the first and last hours are never cut.
                    let left = min(max(x(hour * 60, in: size.width) - 6, 0), size.width - 12)
                    context.draw(label, at: CGPoint(x: left, y: trackHeight + 7), anchor: .topLeading)
                }
            }
            // "Now" is its own canvas, so a tinted face colours it.
            Canvas { context, size in
                let now = x(moment.minute, in: size.width)
                var marker = Path()
                marker.addRect(CGRect(x: now - 1, y: 1.5, width: 2, height: trackHeight + 5))
                marker.addEllipse(in: CGRect(x: now - 2, y: 0, width: 4, height: 4))
                context.fill(marker, with: .color(WatchTheme.amber))
            }
            .widgetAccentable()
        }
        .frame(height: Self.height)
    }

    private func x(_ minute: Int, in width: CGFloat) -> CGFloat {
        CGFloat(minute - moment.windowStart) / CGFloat(TimelineEntries.windowMinutes) * width
    }

    private var hours: [Int] {
        let first = (moment.windowStart + 59) / 60
        return Array(first...(first + 4)).filter { $0 * 60 <= moment.windowStart + TimelineEntries.windowMinutes }
    }

    private func draw(_ row: WatchRow, from left: CGFloat, to right: CGFloat, in context: inout GraphicsContext) {
        let opacity = row.done ? 0.25 : 1
        let bar = CGRect(x: left, y: 3, width: max(right - left, 2), height: trackHeight)
        switch row.kind {
        case .task:
            context.fill(Path(roundedRect: bar, cornerRadius: 1.5), with: .style(.primary.opacity(opacity)))
        case .calendar:
            context.stroke(Path(roundedRect: bar, cornerRadius: 1.5), with: .color(WatchTheme.cal.opacity(opacity)), lineWidth: 1.2)
        case .reminder:
            var tick = Path()
            tick.addRect(CGRect(x: left, y: 1, width: 1.5, height: trackHeight + 4))
            tick.addRect(CGRect(x: left - 1.75, y: -1, width: 5, height: 2))
            context.fill(tick, with: .style(.primary.opacity(opacity)))
        }
    }
}

// MARK: Capture (V48 §7.2)

struct CaptureEntry: TimelineEntry {
    var date: Date
}

/// Shows no data, so it never goes stale and spends no refreshes.
struct CaptureProvider: TimelineProvider {
    func placeholder(in context: Context) -> CaptureEntry { CaptureEntry(date: Date()) }

    func getSnapshot(in context: Context, completion: @escaping (CaptureEntry) -> Void) {
        completion(CaptureEntry(date: Date()))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<CaptureEntry>) -> Void) {
        completion(Timeline(entries: [CaptureEntry(date: Date())], policy: .never))
    }
}

struct CaptureComplication: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WatchEnvironment.captureKind, provider: CaptureProvider()) { _ in
            CaptureGlyph()
                .containerBackground(for: .widget) { Color.clear }
                .widgetURL(WatchEnvironment.captureURL)
        }
        .configurationDisplayName("Capture")
        .description("Opens Thock already listening.")
        .supportedFamilies([.accessoryCircular, .accessoryCorner])
    }
}

struct CaptureGlyph: View {
    @Environment(\.widgetFamily) private var family

    var body: some View {
        if family == .accessoryCorner {
            Image(systemName: "mic.fill")
                .font(.system(size: 20, weight: .semibold))
                .foregroundStyle(WatchTheme.amber)
                .widgetAccentable()
                .widgetLabel {
                    Text("CAPTURE")
                }
                .accessibilityLabel("Capture")
        } else {
            ZStack {
                AccessoryWidgetBackground()
                Image(systemName: "mic.fill")
                    .font(.system(size: 22, weight: .semibold))
                    .foregroundStyle(WatchTheme.amber)
                    .widgetAccentable()
            }
            .accessibilityLabel("Capture")
        }
    }
}

// MARK: The Capture control (V48 §7.4)

@available(watchOS 26, *)
struct CaptureControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: "com.thethock.ios.watchkitapp.capture") {
            ControlWidgetButton(action: CaptureIntent()) {
                Label("Capture", systemImage: "mic.fill")
            }
        }
        .displayName("Capture")
        .description("Opens Thock already listening.")
    }
}
