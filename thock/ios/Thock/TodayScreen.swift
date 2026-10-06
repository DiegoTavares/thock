import SwiftUI
import ThockKit

/// The home screen is today's note, drawn as cards from the note's own
/// sections, with the compose dock always present (V33 §5).
struct TodayScreen: View {
    @Environment(AppModel.self) private var model

    private var days: [VaultDay] {
        VaultDay.pages(around: model.dayPagesAnchor, today: .today())
    }

    private var weeks: [VaultWeek] {
        VaultWeek.pages(around: model.weekPagesAnchor, current: .current())
    }

    var body: some View {
        @Bindable var model = model
        VStack(spacing: 0) {
            if model.isUnlocked {
                Group {
                    if model.showingWeek {
                        TabView(selection: $model.selectedWeek) {
                            ForEach(weeks, id: \.self) { week in
                                WeekCanvas(week: week)
                                    .tag(week)
                            }
                        }
                    } else {
                        TabView(selection: $model.selectedDay) {
                            ForEach(days, id: \.self) { day in
                                DayCanvas(day: day)
                                    .tag(day)
                            }
                        }
                    }
                }
                .tabViewStyle(.page(indexDisplayMode: .never))
                .overlay(alignment: .topTrailing) { YouButton() }
            } else {
                LockedView()
            }
            if model.syncState == .disconnected {
                NoticeBar(text: "This phone is no longer connected to your desk.", action: "Connect again") {
                    Task { await model.disconnect() }
                }
            } else if model.isReadOnly {
                NoticeBar(text: "Thock Plus has ended, so this phone is read-only.", action: "Renew Thock Plus") {
                    if let url = URL(string: "https://thethock.com") {
                        UIApplication.shared.open(url)
                    }
                }
            } else {
                ComposeDock()
            }
        }
        .background(Theme.ground)
    }
}

struct YouButton: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Button {
            model.sheet = .you
        } label: {
            Image(systemName: "ellipsis")
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(Theme.muted)
                .frame(width: 44, height: 44)
                .overlay(alignment: .topTrailing) {
                    if model.waitingForDesk > 0 {
                        Circle().fill(Theme.amber).frame(width: 7, height: 7).offset(x: -8, y: 10)
                    }
                }
        }
        .padding(.trailing, 10)
        .padding(.top, 2)
        .accessibilityLabel(model.waitingForDesk > 0 ? "You, \(model.waitingForDesk) waiting for the desk" : "You")
    }
}

struct LockedView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: 14) {
            Spacer()
            Text("Thock")
                .font(Theme.serif(40, style: .largeTitle))
                .foregroundStyle(Theme.ink)
            Text("Your notes are waiting behind Face ID.\nYou can still write something down.")
                .font(.system(size: 16))
                .foregroundStyle(Theme.muted)
                .multilineTextAlignment(.center)
            Button("Show my notes") {
                Task { await model.unlock() }
            }
            .font(.system(size: 17, weight: .semibold))
            .foregroundStyle(Theme.amber)
            .padding(.top, 6)
            Spacer()
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 32)
    }
}

struct NoticeBar: View {
    var text: String
    var action: String
    var onAction: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            Hairline()
            VStack(alignment: .leading, spacing: 6) {
                Text(text)
                    .font(.system(size: 15))
                    .foregroundStyle(Theme.muted)
                Button(action, action: onAction)
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(Theme.amber)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 20)
            .padding(.vertical, 14)
        }
        .background(Theme.surface)
    }
}

struct ComposeDock: View {
    @Environment(AppModel.self) private var model

    /// The field reaches into the home indicator's band (the window's inset,
    /// which leaves the keyboard out), so it has about as much room below it
    /// as above instead of floating over an empty strip.
    private var bottomPadding: CGFloat {
        let inset = UIApplication.shared.connectedScenes
            .compactMap { ($0 as? UIWindowScene)?.keyWindow?.safeAreaInsets.bottom }
            .first ?? 0
        return inset > 0 ? min(0, 20 - inset) : 12
    }

    var body: some View {
        VStack(spacing: 0) {
            Hairline()
            Button {
                model.sheet = .capture(entry: "dock", preset: nil)
            } label: {
                Text("Write something…")
                    .font(.system(size: 16))
                    .foregroundStyle(Theme.dim)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16)
                    .frame(height: 44)
                    .background(Theme.ground, in: Capsule())
                    .overlay(Capsule().stroke(Theme.rule, lineWidth: 1))
                    .contentShape(Capsule())
                    .padding(.horizontal, 18)
                    .padding(.top, 12)
                    .padding(.bottom, bottomPadding)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Write something")
        }
        .background(Theme.surface.ignoresSafeArea(edges: .bottom))
    }
}

/// One day: the header, the quick actions on today, and a card per section.
struct DayCanvas: View {
    @Environment(AppModel.self) private var model
    var day: VaultDay

    private var isToday: Bool { day == .today() }

    private var eyebrow: String {
        let offset = VaultDay.today().days(until: day)
        let name: String
        switch offset {
        case 0: name = "Today"
        case -1: name = "Yesterday"
        case 1: name = "Tomorrow"
        default: name = offset < 0 ? "\(-offset) days ago" : "In \(offset) days"
        }
        return "\(name) · week \(day.isoWeek)"
    }

    /// A day before the oldest note the phone holds is one sync has not
    /// brought down yet, not one that was never written (V37 §11 #7).
    private var emptyText: String {
        if isToday { return "Nothing written today yet." }
        if let oldest = model.session?.oldestDay(), day < oldest { return "Not on this phone yet." }
        return "No note for this day."
    }

    var body: some View {
        let _ = model.revision
        let view = model.session?.view(day)
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                NoteHeader(eyebrow: eyebrow, title: day.formatted("dddd, MMMM D"))

                if isToday {
                    QuickActions()
                }

                if let view {
                    let inboxAfter = view.cards.last { $0.kind == .planner || $0.kind == .journal }?.id
                    ForEach(view.cards) { card in
                        CardView(card: card, view: view, note: .day(day))
                        if isToday, card.id == inboxAfter {
                            InboxRow()
                        }
                    }
                    if isToday, inboxAfter == nil {
                        InboxRow()
                    }
                } else {
                    Hairline()
                    Text(emptyText)
                        .font(.system(size: 16))
                        .foregroundStyle(Theme.dim)
                    if isToday {
                        InboxRow()
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 22)
            .padding(.bottom, 28)
        }
        .scrollDismissesKeyboard(.interactively)
    }
}

/// The eyebrow and the title in Petrona, and the door to the calendar
/// (V37 §4.1): the same header on a day and on a week.
struct NoteHeader: View {
    @Environment(AppModel.self) private var model
    var eyebrow: String
    var title: String

    var body: some View {
        Button {
            model.openCalendar()
        } label: {
            VStack(alignment: .leading, spacing: 4) {
                Text(eyebrow.uppercased())
                    .font(Theme.label())
                    .tracking(0.9)
                    .foregroundStyle(Theme.dim)
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(title)
                        .font(Theme.serif(30, style: .title1))
                        .foregroundStyle(Theme.ink)
                    Image(systemName: "chevron.down")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Theme.dim)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.top, 14)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
        .accessibilityHint("Opens the calendar")
    }
}

/// One week: the header, the day strip, and a card per section of the
/// weekly note (V37 §5).
struct WeekCanvas: View {
    @Environment(AppModel.self) private var model
    var week: VaultWeek

    private var eyebrow: String {
        let offset = VaultWeek.current().weeks(until: week)
        let name: String
        switch offset {
        case 0: name = "This week"
        case -1: name = "Last week"
        case 1: name = "Next week"
        default: name = offset < 0 ? "\(-offset) weeks ago" : "In \(offset) weeks"
        }
        let monday = week.monday
        let sunday = week.sunday
        let range = monday.month == sunday.month
            ? "\(monday.formatted("MMM D")) – \(sunday.formatted("D"))"
            : "\(monday.formatted("MMM D")) – \(sunday.formatted("MMM D"))"
        return "\(name) · \(range)"
    }

    var body: some View {
        let _ = model.revision
        let view = model.session?.view(.week(week))
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                NoteHeader(eyebrow: eyebrow, title: "Week \(week.week)")
                DayStrip(week: week)
                if let view {
                    ForEach(view.cards) { card in
                        CardView(card: card, view: view, note: .week(week))
                    }
                } else {
                    Hairline()
                    Text("No note for this week.")
                        .font(.system(size: 16))
                        .foregroundStyle(Theme.dim)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 22)
            .padding(.bottom, 28)
        }
        .scrollDismissesKeyboard(.interactively)
    }
}

/// Monday to Sunday under the week's title; a tap jumps to that day's
/// canvas. Today is amber, a day without a note is dim.
struct DayStrip: View {
    @Environment(AppModel.self) private var model
    var week: VaultWeek

    var body: some View {
        let today = VaultDay.today()
        let written = model.session?.daysWithNotes(in: VaultMonth(week.monday)).union(model.session?.daysWithNotes(in: VaultMonth(week.sunday)) ?? []) ?? []
        HStack(spacing: 0) {
            ForEach(week.days, id: \.self) { day in
                let isToday = day == today
                let hasNote = written.contains(day)
                Button {
                    model.go(to: day)
                } label: {
                    VStack(spacing: 2) {
                        Text(String(day.weekdayName.prefix(1)))
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(Theme.dim)
                        Text("\(day.day)")
                            .font(Theme.serif(17, style: .body))
                            .foregroundStyle(isToday ? Theme.amber : (hasNote ? Theme.ink : Theme.dim))
                        Circle()
                            .fill(hasNote ? Theme.cal : .clear)
                            .frame(width: 4, height: 4)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 4)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("\(day.weekdayName) \(day.day)\(isToday ? ", today" : "")\(hasNote ? ", has a note" : "")")
            }
        }
    }
}

struct QuickActions: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(spacing: 8) {
            action("+", "Idea") { model.open(.idea) }
            action("¶", "Journal") { model.open(.journal) }
            action("⇗", "Clip") { model.open(.clip) }
            action("✦", "Ask") { model.open(.ask) }
        }
        .disabled(model.isReadOnly)
        .opacity(model.isReadOnly ? 0.5 : 1)
    }

    private func action(_ glyph: String, _ title: String, perform: @escaping () -> Void) -> some View {
        Button(action: perform) {
            VStack(spacing: 4) {
                Text(glyph)
                    .font(Theme.mono(17))
                    .foregroundStyle(Theme.amber)
                Text(title)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(Theme.ink)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 9)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: 14))
            .overlay(RoundedRectangle(cornerRadius: 14).stroke(Theme.rule, lineWidth: 1))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title)
    }
}

/// The Inbox row: how many captures wait for the desk, and the way into the
/// feed with their receipts.
struct InboxRow: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let _ = model.revision
        let waiting = model.store?.waitingInboxNotes().count ?? 0
        Button {
            model.sheet = .receipts
        } label: {
            VStack(alignment: .leading, spacing: 0) {
                Hairline()
                HStack(alignment: .firstTextBaseline) {
                    Text("INBOX")
                        .font(Theme.label())
                        .tracking(1.1)
                        .foregroundStyle(Theme.dim)
                    Spacer()
                    Text(waiting == 0 ? "nothing waiting" : "\(waiting) waiting for the desk")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(waiting == 0 ? Theme.dim : Theme.amber)
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
        .accessibilityLabel(waiting == 0 ? "Inbox, nothing waiting" : "Inbox, \(waiting) waiting for the desk")
    }
}
