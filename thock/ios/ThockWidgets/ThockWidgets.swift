import AppIntents
import SwiftUI
import ThockKit
import WidgetKit

@main
struct ThockWidgetBundle: WidgetBundle {
    var body: some Widget {
        TodayWidget()
        IdeaControl()
        JournalControl()
    }
}

struct PlanLine: Identifiable {
    var id: String { hash + String(ordinal) }
    var hash: String
    var ordinal: Int
    var time: String
    var label: String
    var isCalendar: Bool
}

struct PlanEntry: TimelineEntry {
    var date: Date
    var day: VaultDay
    var lines: [PlanLine]
    var connected: Bool
    var remaining: Int
}

struct PlanProvider: TimelineProvider {
    func placeholder(in context: Context) -> PlanEntry {
        PlanEntry(date: Date(), day: .today(), lines: [
            PlanLine(hash: "a", ordinal: 0, time: "09:30", label: "Deep work", isCalendar: false),
            PlanLine(hash: "b", ordinal: 0, time: "12:30", label: "Lunch", isCalendar: true),
            PlanLine(hash: "c", ordinal: 0, time: "", label: "Buy a card", isCalendar: false),
        ], connected: true, remaining: 3)
    }

    func entry() -> PlanEntry {
        guard let store = try? ThockEnvironment.openStore(), store.isConnected else {
            return PlanEntry(date: Date(), day: .today(), lines: [], connected: false, remaining: 0)
        }
        let session = VaultSession(store: store)
        let open = session.nextLines(limit: 50)
        let lines = open.prefix(3).map { item in
            PlanLine(hash: item.hash, ordinal: item.ordinal,
                     time: item.time.map { String(format: "%02d:%02d", $0.startMinutes / 60, $0.startMinutes % 60) } ?? "",
                     label: Inline.plainText(item.label), isCalendar: item.isCalendar)
        }
        return PlanEntry(date: Date(), day: session.today(), lines: Array(lines), connected: true, remaining: open.count)
    }

    func getSnapshot(in context: Context, completion: @escaping (PlanEntry) -> Void) {
        completion(context.isPreview ? placeholder(in: context) : entry())
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<PlanEntry>) -> Void) {
        // The app reloads the timeline on every change; this refresh only
        // carries the widget across midnight and long quiet stretches.
        let next = Calendar.current.date(byAdding: .minute, value: 30, to: Date()) ?? Date().addingTimeInterval(1800)
        completion(Timeline(entries: [entry()], policy: .after(next)))
    }
}

struct TodayWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: "com.thethock.ios.today", provider: PlanProvider()) { entry in
            TodayWidgetView(entry: entry)
        }
        .configurationDisplayName("Today")
        .description("The next lines of today's plan. Tap a box to tick it.")
        .supportedFamilies([.systemSmall, .systemMedium, .accessoryRectangular])
    }
}

struct TodayWidgetView: View {
    @Environment(\.widgetFamily) private var family
    var entry: PlanEntry

    var body: some View {
        let accessory = family == .accessoryRectangular
        VStack(alignment: .leading, spacing: accessory ? 1 : 6) {
            if !accessory {
                Text("THOCK · TODAY")
                    .font(.system(size: 10, weight: .semibold))
                    .tracking(0.9)
                    .foregroundStyle(Theme.dim)
            }
            if !entry.connected {
                Text("Open Thock to connect your desk.")
                    .font(.system(size: 13))
                    .foregroundStyle(accessory ? Color.primary : Theme.muted)
            } else if entry.lines.isEmpty {
                Text("Nothing left on today's plan.")
                    .font(.system(size: 13))
                    .foregroundStyle(accessory ? Color.primary : Theme.muted)
            } else {
                ForEach(entry.lines) { line in
                    row(line, accessory: accessory)
                }
            }
            if !accessory {
                Spacer(minLength: 0)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .containerBackground(for: .widget) { Theme.ground }
        .widgetURL(URL(string: "thock://today"))
    }

    @ViewBuilder
    private func row(_ line: PlanLine, accessory: Bool) -> some View {
        HStack(spacing: accessory ? 5 : 8) {
            if line.isCalendar {
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(accessory ? Color.primary : Theme.cal)
                    .frame(width: 3, height: 14)
                    .padding(.horizontal, accessory ? 5 : 6.5)
            } else {
                Button(intent: TickLineIntent(hash: line.hash, ordinal: line.ordinal, day: entry.day)) {
                    RoundedRectangle(cornerRadius: 4.5)
                        .stroke(accessory ? Color.primary : Theme.muted, lineWidth: 1.4)
                        .frame(width: accessory ? 13 : 16, height: accessory ? 13 : 16)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Tick \(line.label)")
            }
            if !line.time.isEmpty, family != .systemSmall {
                Text(line.time)
                    .font(.system(size: accessory ? 11 : 11.5, design: .monospaced))
                    .foregroundStyle(accessory ? Color.secondary : Theme.muted)
            }
            Text(line.label)
                .font(.system(size: accessory ? 13 : 14))
                .foregroundStyle(accessory ? Color.primary : Theme.ink)
                .lineLimit(1)
                .privacySensitive()
        }
    }
}

/// Lock Screen and Control Center controls (iOS 18): straight into a sheet.
struct IdeaControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: "com.thethock.ios.control.idea") {
            ControlWidgetButton(action: NewIdeaIntent()) {
                Label("Idea", systemImage: "plus")
            }
        }
        .displayName("Idea")
        .description("A blank capture, keyboard up.")
    }
}

struct JournalControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: "com.thethock.ios.control.journal") {
            ControlWidgetButton(action: JournalIntent()) {
                Label("Journal", systemImage: "text.alignleft")
            }
        }
        .displayName("Journal")
        .description("Today's journal, with a new entry started.")
    }
}
