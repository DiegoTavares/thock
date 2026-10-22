import SwiftUI
import ThockKit

/// How a reminder's chip reads in a line of text (V45 §8.4): *⏰ Today 14:30*
/// in amber, overdue in the warning colour, closed muted.
enum ReminderChipText {
    static func color(_ state: ReminderChipState) -> Color {
        switch state {
        case .upcoming, .today: return Theme.amber
        case .overdue: return Theme.warn
        case .closed, .noDay: return Theme.dim
        }
    }

    static func text(_ chip: ReminderChip) -> Text {
        Text("⏰ \(chip.label)")
            .font(.system(size: 13, weight: .medium))
            .foregroundStyle(color(chip.state))
    }
}

/// The Reminders screen (V45 §10.4): Overdue, Today and Upcoming from the
/// index, with Done folded. It opens over the pager the way the backlog
/// does. Swipe right closes, swipe left reschedules, a long press offers the
/// rest; each is one write on the home line, with Undo.
struct RemindersScreen: View {
    @Environment(AppModel.self) private var model
    @State private var showingDone = false
    @State private var rescheduling: IndexedReminder?

    var body: some View {
        let _ = model.revision
        let today = VaultDay.today()
        let session = model.session
        let index = session?.reminderIndex() ?? ReminderIndex(config: VaultConfig()) { _ in nil }
        let dateOnlyAt = session?.config.reminders.dateOnlyAt ?? ReminderSettings().dateOnlyAt
        let groups = ReminderGroup.allCases.map { ($0, index.reminders(in: $0, today: today, dateOnlyAt: dateOnlyAt)) }
        let open = groups.filter { $0.0 != .done }
        let done = groups.first { $0.0 == .done }?.1 ?? []
        // A time with no day (a loose `(@17:00)`) never rings; it is listed
        // so it can be given one.
        let noDay = index.reminders.filter { $0.status == .open && $0.due == nil && $0.reminder?.token != nil }
        VStack(alignment: .leading, spacing: 0) {
            header(Self.counts(index, today: today, dateOnlyAt: dateOnlyAt))
            List {
                if open.allSatisfy({ $0.1.isEmpty }), noDay.isEmpty {
                    Text("Nothing due. A capture with the Reminder chip, or Remind me… on a line, sets one.")
                        .font(.system(size: 16))
                        .foregroundStyle(Theme.dim)
                        .padding(.vertical, 12)
                        .plainRow()
                }
                ForEach(open, id: \.0) { group, reminders in
                    if !reminders.isEmpty {
                        sectionLabel(Self.title(group), count: reminders.count)
                        ForEach(reminders) { reminder in
                            row(reminder, today: today)
                        }
                    }
                }
                if !noDay.isEmpty {
                    sectionLabel("Which day?", count: noDay.count)
                    ForEach(noDay) { reminder in
                        row(reminder, today: today)
                    }
                }
                Button {
                    withAnimation(.snappy) { showingDone.toggle() }
                } label: {
                    HStack(alignment: .firstTextBaseline) {
                        CardLabel(title: "Done", note: Self.doneNote(done, today: today))
                        Image(systemName: showingDone ? "chevron.down" : "chevron.right")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(Theme.dim)
                    }
                    .padding(.top, 22)
                    .padding(.bottom, 6)
                    .overlay(alignment: .bottom) { Hairline() }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .plainRow()
                .accessibilityLabel("Done, \(done.count)")
                .accessibilityHint(showingDone ? "Hides the finished reminders" : "Shows the finished reminders")
                if showingDone {
                    ForEach(done) { reminder in
                        row(reminder, today: today)
                    }
                }
                Color.clear.frame(height: 24).plainRow()
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .environment(\.defaultMinListRowHeight, 1)
        }
        .background(Theme.ground)
        .sheet(item: $rescheduling) { reminder in
            RemindMeSheet(subject: reminder.label, initial: reminder.due) { due in
                model.rescheduleReminder(reminder, to: due)
            }
            .presentationDetents([.height(360)])
            .presentationBackground(Theme.surface)
            .presentationCornerRadius(26)
            .preferredColorScheme(model.appearance.scheme)
        }
    }

    private func header(_ counts: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Button {
                    model.closeReminders()
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "chevron.left")
                            .font(.system(size: 14, weight: .semibold))
                        Text("Today")
                    }
                    .font(.system(size: 17))
                    .foregroundStyle(Theme.amber)
                    .frame(minHeight: 44)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Back to today")
                Spacer()
            }
            .padding(.top, 6)
            Text("Reminders")
                .font(Theme.serif(30, style: .title1))
                .foregroundStyle(Theme.ink)
            Text(counts.uppercased())
                .font(Theme.label())
                .tracking(0.9)
                .foregroundStyle(Theme.dim)
                .padding(.bottom, 6)
        }
        .padding(.horizontal, 22)
    }

    private func sectionLabel(_ title: String, count: Int) -> some View {
        CardLabel(title: title, note: "\(count)")
            .padding(.top, 18)
            .padding(.bottom, 6)
            .overlay(alignment: .bottom) { Hairline() }
            .accessibilityAddTraits(.isHeader)
            .plainRow()
    }

    @ViewBuilder
    private func row(_ reminder: IndexedReminder, today: VaultDay) -> some View {
        let editable = !model.isReadOnly && reminder.line != nil
        let closed = reminder.status != .open
        ReminderRow(reminder: reminder, today: today)
            .plainRow()
            .swipeActions(edge: .leading, allowsFullSwipe: true) {
                if editable {
                    if closed {
                        if reminder.status == .closed {
                            Button("Reopen") { model.reopenReminder(reminder) }
                                .tint(Theme.amber)
                        }
                    } else {
                        Button("Done") { model.closeReminder(reminder) }
                            .tint(Theme.good)
                    }
                }
            }
            .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                if editable, reminder.status != .removed {
                    Button("Reschedule") { rescheduling = reminder }
                        .tint(Theme.amber)
                }
            }
            .contextMenu {
                if editable {
                    if reminder.status == .open {
                        Button("Done") { model.closeReminder(reminder) }
                    } else if reminder.status == .closed {
                        Button("Reopen") { model.reopenReminder(reminder) }
                    }
                    Button("Reschedule…") { rescheduling = reminder }
                }
                Button("Open note") { model.openNote(of: reminder) }
                if editable, reminder.reminder?.token != nil {
                    Button("Drop the reminder", role: .destructive) { model.dropReminder(reminder) }
                }
            }
    }

    static func title(_ group: ReminderGroup) -> String {
        switch group {
        case .overdue: return "Overdue"
        case .today: return "Today"
        case .upcoming: return "Upcoming"
        case .done: return "Done"
        }
    }

    /// *2 overdue · 3 today*, or *nothing due*.
    static func counts(_ index: ReminderIndex, today: VaultDay, dateOnlyAt: ClockTime) -> String {
        let overdue = index.reminders(in: .overdue, today: today, dateOnlyAt: dateOnlyAt).count
        let due = index.reminders(in: .today, today: today, dateOnlyAt: dateOnlyAt).count
        var parts: [String] = []
        if overdue > 0 { parts.append("\(overdue) overdue") }
        if due > 0 { parts.append("\(due) today") }
        return parts.isEmpty ? "nothing due" : parts.joined(separator: " · ")
    }

    /// *2 this week*: how many finished this ISO week, by their Done stamp
    /// or, without one, their due.
    static func doneNote(_ done: [IndexedReminder], today: VaultDay) -> String? {
        guard !done.isEmpty else { return nil }
        let week = VaultWeek(today)
        let thisWeek = done.filter { reminder in
            switch reminder.stamp {
            case .done(let day)?, .dropped(let day)?, .removed(let day)?: return week.contains(day)
            case nil: return reminder.due.map { week.contains($0.date) } ?? false
            }
        }.count
        return thisWeek > 0 ? "\(thisWeek) this week" : "\(done.count)"
    }
}

/// One reminder: its words, its chip, and the note it lives in.
struct ReminderRow: View {
    var reminder: IndexedReminder
    var today: VaultDay

    var body: some View {
        let closed = reminder.status != .open
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: closed ? "checkmark.circle.fill" : "bell")
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(closed ? Theme.dim : Theme.amber)
                .frame(width: 20)
            VStack(alignment: .leading, spacing: 3) {
                Text(Rich.text(Inline.parse(reminder.label)))
                    .font(.system(size: 17))
                    .foregroundStyle(closed ? Theme.dim : Theme.ink)
                    .strikethrough(closed, color: Theme.dim)
                    .fixedSize(horizontal: false, vertical: true)
                (chip + Text("  ") + Text(home).font(.system(size: 13)).foregroundStyle(Theme.dim))
                    .lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.vertical, 8)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }

    private var chip: Text {
        switch reminder.status {
        case .removed: return Text("removed").font(.system(size: 13, weight: .medium)).foregroundStyle(Theme.dim)
        case .dropped: return Text("dropped").font(.system(size: 13, weight: .medium)).foregroundStyle(Theme.dim)
        case .missing: return Text("line not found").font(.system(size: 13, weight: .medium)).foregroundStyle(Theme.dim)
        case .open, .closed:
            guard let line = reminder.reminder, let chip = ReminderChip(line, today: today) else { return Text("") }
            return ReminderChipText.text(chip)
        }
    }

    private var home: String {
        Reminders.noteLink(reminder.home)
    }
}

/// *REMINDERS · 2 overdue · 3 today ›* under the planner card on Today.
struct RemindersRowOnToday: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let _ = model.revision
        let today = VaultDay.today()
        let counts = model.session.map { session in
            RemindersScreen.counts(session.reminderIndex(), today: today, dateOnlyAt: session.config.reminders.dateOnlyAt)
        } ?? "nothing due"
        Button {
            model.openReminders()
        } label: {
            VStack(alignment: .leading, spacing: 0) {
                Hairline()
                HStack(alignment: .firstTextBaseline) {
                    Text("REMINDERS")
                        .font(Theme.label())
                        .tracking(1.1)
                        .foregroundStyle(Theme.dim)
                    Spacer()
                    Text(counts)
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(counts.contains("overdue") ? Theme.warn : (counts == "nothing due" ? Theme.dim : Theme.amber))
                    Image(systemName: "chevron.right")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.dim)
                }
                .padding(.top, 10)
                .padding(.bottom, 2)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Reminders, \(counts)")
    }
}

extension View {
    /// A list row drawn on the canvas, with the backlog's insets.
    func plainRow() -> some View {
        listRowBackground(Theme.ground)
            .listRowSeparator(.hidden)
            .listRowInsets(EdgeInsets(top: 0, leading: 22, bottom: 0, trailing: 22))
    }
}
