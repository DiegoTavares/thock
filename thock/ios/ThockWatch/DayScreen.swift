import SwiftUI
import ThockKit

/// The day's planner lines, calendar events and reminders (V48 §9). The
/// Digital Crown scrolls, the chevrons or a swipe move a day, and a tap on a
/// box ticks the line.
struct DayScreen: View {
    @Environment(WatchModel.self) private var model

    var body: some View {
        @Bindable var model = model
        NavigationStack {
            List {
                header
                    .listRowBackground(Color.clear)
                content
                if model.outbox.waitingCount > 0 {
                    Button {
                        model.showingOutbox = true
                    } label: {
                        Text("\(model.outbox.waitingCount) waiting for your iPhone ›")
                            .font(.footnote)
                            .foregroundStyle(WatchTheme.muted)
                    }
                }
            }
            .simultaneousGesture(DragGesture(minimumDistance: 30).onEnded { value in
                guard abs(value.translation.width) > abs(value.translation.height) * 2 else { return }
                move(value.translation.width < 0 ? 1 : -1)
            })
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button {
                        model.startCapture(voice: true)
                    } label: {
                        Label("Capture", systemImage: "mic.fill")
                            .labelStyle(.iconOnly)
                            .foregroundStyle(WatchTheme.amber)
                    }
                    // The app's amber tint would fill the button's glass on
                    // watchOS 26 and hide an amber icon inside it.
                    .tint(nil)
                }
            }
            .navigationDestination(isPresented: $model.showingOutbox) {
                OutboxScreen()
            }
            .sheet(isPresented: $model.showingCapture) {
                CaptureScreen()
            }
            .alert(model.notice ?? "", isPresented: Binding(get: { model.notice != nil }, set: { if !$0 { model.notice = nil } })) {
                Button("OK", role: .cancel) {}
            }
            .task(id: model.day) { model.ask(model.day) }
        }
        .background(WatchTheme.ground)
    }

    private func move(_ days: Int) {
        model.day = model.day.adding(days: days)
    }

    private var header: some View {
        VStack(spacing: 2) {
            HStack {
                Button { move(-1) } label: {
                    Image(systemName: "chevron.left")
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Previous day")
                Spacer(minLength: 4)
                VStack(spacing: 0) {
                    Text(model.day.formatted("ddd D"))
                        .font(WatchTheme.serif(22))
                        .foregroundStyle(WatchTheme.ink)
                    Text(subtitle)
                        .font(.caption2)
                        .foregroundStyle(model.day == model.today ? WatchTheme.amber : WatchTheme.muted)
                }
                Spacer(minLength: 4)
                Button { move(1) } label: {
                    Image(systemName: "chevron.right")
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Next day")
            }
            .foregroundStyle(WatchTheme.muted)
            if let since = model.staleSince {
                Text("As of \(Self.clock(since))")
                    .font(.caption2)
                    .foregroundStyle(WatchTheme.dim)
            }
            if model.day != model.today {
                Button("Today") { model.day = model.today }
                    .font(.footnote)
                    .foregroundStyle(WatchTheme.amber)
                    .buttonStyle(.plain)
                    .padding(.top, 2)
            }
        }
        .frame(maxWidth: .infinity)
    }

    private var subtitle: String {
        switch model.today.days(until: model.day) {
        case 0: return "Today"
        case -1: return "Yesterday"
        case 1: return "Tomorrow"
        default: return model.day.weekdayName
        }
    }

    static func clock(_ date: Date) -> String {
        let parts = Calendar.current.dateComponents([.hour, .minute], from: date)
        return ClockTime(clampingHour: parts.hour ?? 0, minute: parts.minute ?? 0).description
    }

    @ViewBuilder
    private var content: some View {
        if model.needsUpdate {
            message(WatchOutbox.updateMessage)
        } else if model.snapshot == nil || model.snapshot?.state == .notConnected {
            message("Open Thock on your iPhone to connect")
        } else if let reading = model.reading(model.day) {
            rows(reading)
        } else if model.isOutOfRange(model.day) {
            if model.asking(model.day) {
                ProgressView()
                    .listRowBackground(Color.clear)
            } else {
                message("Open Thock on your iPhone to see this day")
            }
        } else {
            message("Nothing planned")
        }
    }

    private func message(_ text: String) -> some View {
        Text(text)
            .font(.footnote)
            .foregroundStyle(WatchTheme.muted)
            .listRowBackground(Color.clear)
    }

    @ViewBuilder
    private func rows(_ reading: WatchDay) -> some View {
        let timed = reading.rows.filter { $0.start != nil }.sorted { ($0.start ?? 0) < ($1.start ?? 0) }
        let anytime = reading.rows.filter { $0.start == nil }
        if !reading.exists || reading.rows.isEmpty {
            message("Nothing planned")
        }
        ForEach(Array(timed.enumerated()), id: \.offset) { _, row in
            DayRow(row: row, day: model.day)
        }
        if !anytime.isEmpty {
            Section {
                ForEach(Array(anytime.enumerated()), id: \.offset) { _, row in
                    DayRow(row: row, day: model.day)
                }
            } header: {
                Text("Anytime")
                    .font(.caption2)
                    .foregroundStyle(WatchTheme.dim)
            }
        }
        if reading.notes > 0 {
            Text(reading.notes == 1 ? "+1 note" : "+\(reading.notes) notes")
                .font(.caption2)
                .foregroundStyle(WatchTheme.dim)
                .listRowBackground(Color.clear)
        }
    }
}

/// One line of the day: its time, its words, and a box to tick unless it
/// is a calendar event.
struct DayRow: View {
    @Environment(WatchModel.self) private var model
    var row: WatchRow
    var day: VaultDay

    var body: some View {
        let done = model.isDone(row, on: day)
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            if row.kind == .calendar {
                RoundedRectangle(cornerRadius: 1)
                    .fill(WatchTheme.cal)
                    .frame(width: 3, height: 14)
            } else {
                Button {
                    model.tick(row, on: day)
                } label: {
                    Image(systemName: done ? "checkmark.square.fill" : "square")
                        .foregroundStyle(done ? WatchTheme.dim : WatchTheme.muted)
                }
                .buttonStyle(.plain)
                .disabled(done)
                .accessibilityLabel(done ? "Done" : "Tick \(row.label)")
            }
            VStack(alignment: .leading, spacing: 1) {
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    if let start = row.start {
                        Text(ClockTime(clampingHour: start / 60, minute: start % 60).description)
                            .font(WatchTheme.mono(12))
                            .foregroundStyle(row.kind == .calendar ? WatchTheme.cal : WatchTheme.muted)
                    }
                    if row.kind == .reminder {
                        Image(systemName: "bell.fill")
                            .font(.system(size: 10))
                            .foregroundStyle(WatchTheme.amber)
                            .accessibilityLabel("Reminder")
                    }
                }
                Text(row.label)
                    .font(.body)
                    .strikethrough(done)
                    .foregroundStyle(done ? WatchTheme.dim : WatchTheme.ink)
                if let carried = row.carriedFrom.flatMap(VaultDay.init(iso:)) {
                    Text("from \(carried.formatted("ddd"))")
                        .font(.caption2)
                        .foregroundStyle(WatchTheme.warn)
                }
            }
        }
    }
}
