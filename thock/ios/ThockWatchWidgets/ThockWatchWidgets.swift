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
        case day(VaultDay, rows: [WatchRow], moment: WatchTimelineEntry)
    }

    var date: Date
    var content: Content
}

struct TodayProvider: TimelineProvider {
    func placeholder(in context: Context) -> TodayEntry {
        let now = Date()
        let rows = [
            WatchRow(hash: "1", ordinal: 0, kind: .task, start: 9 * 60, end: 10 * 60, label: "Write the brief"),
            WatchRow(hash: "2", ordinal: 0, kind: .calendar, start: 11 * 60, end: 12 * 60, label: "Standup"),
            WatchRow(hash: "3", ordinal: 0, kind: .reminder, start: 14 * 60, label: "Dentist"),
        ]
        let moment = TimelineEntries.entries(rows: rows, today: .today(), now: now).first
            ?? WatchTimelineEntry(date: now, minute: 0, windowStart: 0, next: nil, left: 0, anytime: 0)
        return TodayEntry(date: now, content: .day(.today(), rows: rows, moment: moment))
    }

    func getSnapshot(in context: Context, completion: @escaping (TodayEntry) -> Void) {
        completion(entries(now: Date()).first ?? placeholder(in: context))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<TodayEntry>) -> Void) {
        let now = Date()
        // The snapshot already holds tomorrow, so the new day draws without
        // the phone; the watch app reloads this when a snapshot arrives.
        let midnight = Calendar.current.date(byAdding: .day, value: 1, to: Calendar.current.startOfDay(for: now)) ?? now.addingTimeInterval(3600)
        completion(Timeline(entries: entries(now: now), policy: .after(midnight)))
    }

    /// One entry every quarter hour to midnight, so the window and the
    /// header move without a reload (§7.3).
    func entries(now: Date) -> [TodayEntry] {
        let snapshot = WatchFiles(folder: WatchEnvironment.container).snapshot()
        // The watch's date: a snapshot from before midnight still holds it.
        let today = VaultDay(now)
        guard let snapshot, snapshot.state != .notConnected, snapshot.covers(today) else {
            return [TodayEntry(date: now, content: .notConnected)]
        }
        let rows = snapshot.day(today)?.rows ?? []
        return TimelineEntries.entries(rows: rows, today: today, now: now).map { moment in
            TodayEntry(date: moment.date, content: .day(today, rows: rows, moment: moment))
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
        .description("Five hours of today's plan, and what comes next.")
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
        case .day(let day, let rows, let moment):
            if family == .accessoryInline {
                inline(moment)
            } else {
                rectangular(day: day, rows: rows, moment: moment)
            }
        }
    }

    private static func clock(_ minutes: Int) -> String {
        ClockTime(clampingHour: minutes / 60 % 24, minute: minutes % 60).description
    }

    private static func next(_ row: WatchRow) -> String {
        "\(row.start.map(clock) ?? "") \(row.label)"
    }

    private static func counts(_ moment: WatchTimelineEntry) -> String {
        moment.anytime > 0 ? "\(moment.left) left · \(moment.anytime) anytime" : "\(moment.left) left"
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

    private func rectangular(day: VaultDay, rows: [WatchRow], moment: WatchTimelineEntry) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Text(day.formatted("ddd D").uppercased())
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(WatchTheme.amber)
                    .widgetAccentable()
                Spacer(minLength: 2)
                Group {
                    if let next = moment.next {
                        Text(Self.next(next))
                    } else if moment.left > 0 {
                        Text("\(moment.left) left")
                    }
                }
                .font(.system(size: 13, weight: .medium))
                .lineLimit(1)
                .privacySensitive()
            }
            if rows.isEmpty {
                Text("Nothing planned today")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            } else if moment.left > 0 {
                Text(Self.counts(moment))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                TimelineTrack(rows: rows, moment: moment)
            }
        }
    }
}

/// Five hours from the half hour before now: tasks filled, events outlined,
/// reminders a tick and a line, done items faint, now an accent line.
/// Every difference is a shape or an opacity too, for tinted faces.
struct TimelineTrack: View {
    var rows: [WatchRow]
    var moment: WatchTimelineEntry

    private let trackHeight: CGFloat = 10

    var body: some View {
        GeometryReader { geometry in
            let width = geometry.size.width
            let start = moment.windowStart
            let span = CGFloat(TimelineEntries.windowMinutes)
            let x = { (minute: Int) in CGFloat(minute - start) / span * width }
            let nowMinute = moment.minute
            ZStack(alignment: .topLeading) {
                Rectangle()
                    .fill(.secondary.opacity(0.3))
                    .frame(width: width, height: 1)
                    .offset(y: 3 + trackHeight / 2)
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    if let begin = row.start {
                        block(row, from: x(begin), to: x(row.end ?? begin + 30))
                    }
                }
                Rectangle()
                    .fill(WatchTheme.amber)
                    .frame(width: 2, height: trackHeight + 6)
                    .offset(x: x(nowMinute) - 1)
                    .widgetAccentable()
                Circle()
                    .fill(WatchTheme.amber)
                    .frame(width: 5, height: 5)
                    .offset(x: x(nowMinute) - 2.5, y: -2)
                    .widgetAccentable()
                ForEach(hours(from: start), id: \.self) { hour in
                    Text(String(format: "%02d", hour % 24))
                        .font(.system(size: 9, weight: .medium, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .offset(x: x(hour * 60) - 6, y: trackHeight + 7)
                }
            }
            .frame(width: width, alignment: .leading)
            .clipped()
        }
        .frame(height: trackHeight + 18)
    }

    private func hours(from start: Int) -> [Int] {
        let first = (start + 59) / 60
        return Array(first...(first + 4)).filter { $0 * 60 <= start + TimelineEntries.windowMinutes }
    }

    @ViewBuilder
    private func block(_ row: WatchRow, from left: CGFloat, to right: CGFloat) -> some View {
        let opacity = row.done ? 0.25 : 1
        switch row.kind {
        case .task:
            RoundedRectangle(cornerRadius: 2)
                .fill(.primary)
                .frame(width: max(right - left, 2), height: trackHeight)
                .offset(x: left, y: 3)
                .opacity(opacity)
        case .calendar:
            RoundedRectangle(cornerRadius: 2)
                .stroke(WatchTheme.cal, lineWidth: 1.5)
                .frame(width: max(right - left, 2), height: trackHeight)
                .offset(x: left, y: 3)
                .opacity(opacity)
        case .reminder:
            ZStack(alignment: .topLeading) {
                Rectangle()
                    .fill(.primary)
                    .frame(width: 1.5, height: trackHeight + 4)
                    .offset(x: left, y: 1)
                Rectangle()
                    .fill(.primary)
                    .frame(width: 5, height: 2)
                    .offset(x: left - 1.75, y: -1)
            }
            .opacity(opacity)
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
