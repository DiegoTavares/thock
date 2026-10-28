import SwiftUI
import ThockKit

/// *Remind me* (V48 §8.3): the phone's presets from the snapshot's
/// settings, or a date and a time picked by hand. The spoken words are never
/// read for dates.
struct RemindMeScreen: View {
    @Environment(WatchModel.self) private var model
    var onPick: (ReminderDue) -> Void

    private var settings: ReminderSettings { model.settings }
    private var today: VaultDay { model.today }

    var body: some View {
        List {
            // *Later today* only while there is a later today left.
            if Calendar.current.component(.hour, from: Date()) < 16 {
                preset("Later today", "17:00", ReminderDue(date: today, time: ClockTime(clampingHour: 17, minute: 0)))
            }
            preset("Tomorrow", settings.tomorrowAt.description, settings.tomorrowDue(today: today))
            preset("Next week", "Mon \(settings.tomorrowAt)", settings.mondayDue(today: today))
            NavigationLink {
                PickDueScreen(initial: settings.tomorrowDue(today: today), dateOnlyAt: settings.dateOnlyAt, onPick: onPick)
            } label: {
                Text("Date and time…")
            }
        }
        .navigationTitle("Remind me")
    }

    private func preset(_ title: String, _ detail: String, _ due: ReminderDue) -> some View {
        Button {
            onPick(due)
        } label: {
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .foregroundStyle(WatchTheme.ink)
                Text(detail)
                    .font(WatchTheme.mono(12))
                    .foregroundStyle(WatchTheme.amber)
            }
        }
        .accessibilityLabel("\(title), \(detail)")
    }
}

/// A day, then an hour and minute in 24-hour time, or *No time* for a
/// date-only reminder.
struct PickDueScreen: View {
    var initial: ReminderDue
    var dateOnlyAt: ClockTime
    var onPick: (ReminderDue) -> Void

    @State private var day = Date()
    @State private var time = Date()
    @State private var noTime = false

    private var picked: ReminderDue {
        let parts = Calendar.current.dateComponents([.hour, .minute], from: time)
        return ReminderDue(date: VaultDay(day), time: noTime ? nil : ClockTime(clampingHour: parts.hour ?? 9, minute: parts.minute ?? 0))
    }

    /// A time already gone never rings and would be overdue at once.
    private var pickedIsAhead: Bool {
        let due = picked
        let today = VaultDay.today()
        guard let time = due.time else { return due.date >= today }
        return (due.date, time.minutes) > (today, ClockTime.now().minutes)
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                DatePicker("Day", selection: $day, in: Calendar.current.startOfDay(for: Date())..., displayedComponents: .date)
                Toggle("No time", isOn: $noTime)
                if !noTime {
                    DatePicker("Time", selection: $time, displayedComponents: .hourAndMinute)
                }
                if !pickedIsAhead {
                    Text("That time has passed. Pick a later one.")
                        .font(.caption2)
                        .foregroundStyle(WatchTheme.warn)
                }
                Button("Done") { onPick(picked) }
                    .tint(WatchTheme.amber)
                    .disabled(!pickedIsAhead)
            }
        }
        // 24-hour time, as the vault writes it.
        .environment(\.locale, Locale(identifier: "en_GB"))
        .navigationTitle("Date and time")
        .onAppear {
            day = Calendar.current.date(from: DateComponents(year: initial.date.year, month: initial.date.month, day: initial.date.day, hour: 12)) ?? Date()
            let at = initial.time ?? dateOnlyAt
            time = Calendar.current.date(bySettingHour: at.hour, minute: at.minute, second: 0, of: Date()) ?? Date()
        }
    }
}
