import SwiftUI
import ThockKit

/// The calendar (V37 §4): a month of ISO weeks, Monday first, with the
/// week's number in a gutter on the left. A day opens that day's canvas, a
/// week number opens the week's note. It is a map of the notes folder, not
/// a scheduling tool: nothing here writes.
struct CalendarSheet: View {
    @Environment(AppModel.self) private var model

    @State private var month: VaultMonth = VaultMonth(.today())

    private static let dayLetters = ["M", "T", "W", "T", "F", "S", "S"]

    private var today: VaultDay { .today() }

    /// The day or week the calendar was opened from, ringed in amber.
    private var selectedDay: VaultDay? { model.showingWeek ? nil : model.selectedDay }
    private var selectedWeek: VaultWeek? { model.showingWeek ? model.selectedWeek : nil }

    var body: some View {
        let _ = model.revision
        let written = model.session?.daysWithNotes(in: month) ?? []
        VStack(spacing: 10) {
            Capsule().fill(Theme.rule).frame(width: 44, height: 5)
                .padding(.top, 10)
            HStack(alignment: .firstTextBaseline) {
                Text(month.name)
                    .font(Theme.serif(22, style: .title2))
                    .foregroundStyle(Theme.ink)
                Spacer()
                HStack(spacing: 22) {
                    monthButton("chevron.left", "Previous month") { month = month.adding(months: -1) }
                    monthButton("chevron.right", "Next month") { month = month.adding(months: 1) }
                }
            }
            .padding(.horizontal, 22)
            .padding(.top, 4)

            Grid(horizontalSpacing: 2, verticalSpacing: 4) {
                GridRow {
                    Text("")
                        .frame(width: 28)
                    ForEach(Array(Self.dayLetters.enumerated()), id: \.offset) { _, letter in
                        Text(letter)
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(Theme.dim)
                            .frame(maxWidth: .infinity)
                    }
                }
                ForEach(month.weeks, id: \.self) { week in
                    GridRow {
                        weekCell(week)
                        ForEach(week.days, id: \.self) { day in
                            dayCell(day, inMonth: month.contains(day), hasNote: written.contains(day), inSelectedWeek: week == selectedWeek)
                        }
                    }
                }
            }
            .padding(.horizontal, 22)

            Hairline(color: Theme.ruleSoft)
                .padding(.horizontal, 22)
            HStack {
                Button("Today") { model.go(to: today) }
                Spacer()
                Text("a dot is a day with a note")
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.dim)
                Spacer()
                Button("This week") { model.go(to: .current()) }
            }
            .font(.system(size: 15, weight: .semibold))
            .foregroundStyle(Theme.amber)
            .padding(.horizontal, 22)
            Spacer(minLength: 0)
        }
        .onAppear {
            month = VaultMonth(selectedWeek?.monday ?? selectedDay ?? today)
        }
    }

    private func monthButton(_ symbol: String, _ label: String, perform: @escaping () -> Void) -> some View {
        Button(action: perform) {
            Image(systemName: symbol)
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(Theme.amber)
                .frame(width: 32, height: 32)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
    }

    /// The gutter: the week's own number, as its note is named. The whole
    /// cell is the target, not the digits.
    private func weekCell(_ week: VaultWeek) -> some View {
        let isCurrent = week == .current()
        let hasNote = model.session?.hasNote(week) ?? false
        let isSelected = week == selectedWeek
        return Button {
            model.go(to: week)
        } label: {
            Text("\(week.week)")
                .font(Theme.mono(11, weight: isCurrent ? .medium : .regular))
                .foregroundStyle(isCurrent ? Theme.amber : (hasNote ? Theme.ink : Theme.dim))
                .frame(width: 28, height: 34)
                .background(isSelected ? Theme.amberSoft : .clear, in: RoundedRectangle(cornerRadius: 7))
                .overlay(alignment: .trailing) {
                    if !isSelected {
                        Rectangle().fill(Theme.ruleSoft).frame(width: 1).padding(.vertical, 6)
                    }
                }
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Week \(week.week)\(isCurrent ? ", this week" : "")\(hasNote ? ", has a note" : "")")
        .accessibilityHint("Opens the week")
    }

    private func dayCell(_ day: VaultDay, inMonth: Bool, hasNote: Bool, inSelectedWeek: Bool) -> some View {
        let isToday = day == today
        let isSelected = day == selectedDay
        return Button {
            model.go(to: day)
        } label: {
            ZStack {
                if inSelectedWeek {
                    Rectangle().fill(Theme.sunken)
                }
                if isToday {
                    RoundedRectangle(cornerRadius: 7).fill(Theme.amber)
                } else if isSelected {
                    RoundedRectangle(cornerRadius: 7).stroke(Theme.amber, lineWidth: 1.3)
                }
                Text("\(day.day)")
                    .font(.system(size: 15, weight: isToday ? .semibold : .regular))
                    .monospacedDigit()
                    .foregroundStyle(isToday ? Theme.amberInk : (inMonth ? Theme.ink : Theme.dim))
                if hasNote {
                    Circle()
                        .fill(isToday ? Theme.amberInk : Theme.cal)
                        .frame(width: 4, height: 4)
                        .frame(maxHeight: .infinity, alignment: .bottom)
                        .padding(.bottom, 3)
                }
            }
            .frame(maxWidth: .infinity)
            .frame(height: 34)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(day.weekdayName), \(day.monthName) \(day.day)\(isToday ? ", today" : "")\(hasNote ? ", has a note" : "")")
        .accessibilityHint("Opens the day")
    }
}
