import ThockKit
import UIKit
import UserNotifications

/// Hands the soonest reminders to iOS and takes their answers (V45 §10.5).
/// Which reminders, and what changed, is `ReminderSchedule`'s; this holds
/// only the `UNUserNotificationCenter` calls.
@MainActor
final class ReminderNotifier: NSObject, UNUserNotificationCenterDelegate {
    private var center: UNUserNotificationCenter { .current() }

    private var registered: ReminderSettings?

    /// The refresh in flight; the next one waits for it, so two never read
    /// the same pending list and add a request twice.
    private var refreshing: Task<Void, Never>?

    /// The three answers, titled from the vault's settings. Registered on
    /// every launch (it needs no permission, and a notification delivered
    /// before the app runs still finds them) and again when the settings
    /// change.
    func registerCategories(_ settings: ReminderSettings) {
        guard registered != settings else { return }
        registered = settings
        let actions = [
            UNNotificationAction(identifier: ReminderAnswer.done.rawValue, title: "Done", options: []),
            UNNotificationAction(identifier: ReminderAnswer.snooze.rawValue, title: settings.snoozeTitle, options: []),
            UNNotificationAction(identifier: ReminderAnswer.tomorrow.rawValue, title: "Tomorrow \(settings.tomorrowAt)", options: []),
        ]
        center.setNotificationCategories([UNNotificationCategory(identifier: ReminderSchedule.category, actions: actions, intentIdentifiers: [])])
    }


    /// Whether reminders can ring: asked the first time a reminder is made,
    /// never at launch.
    func authorization() async -> UNAuthorizationStatus {
        await center.notificationSettings().authorizationStatus
    }

    /// Asks once; afterwards iOS answers from Settings without asking again.
    func requestAuthorization() async -> Bool {
        (try? await center.requestAuthorization(options: [.alert, .sound, .badge])) ?? false
    }

    /// Brings iOS's pending reminder notifications in line with `alerts`,
    /// replacing only what changed, and takes back delivered ones whose
    /// identifier is not in `open` (the reminder closed, or moved to another
    /// due). Without permission there is nothing to hand over. Refreshes run
    /// one after another.
    func refresh(alerts: [ReminderAlert], open: Set<String>) async {
        let previous = refreshing
        let task = Task { @MainActor in
            await previous?.value
            await self.apply(alerts: alerts, open: open)
        }
        refreshing = task
        await task.value
    }

    private func apply(alerts: [ReminderAlert], open: Set<String>) async {
        let status = await authorization()
        guard status == .authorized || status == .provisional || status == .ephemeral else { return }
        let delivered = await center.deliveredNotifications().map(\.request.identifier)
        let stale = ReminderSchedule.staleDelivered(delivered, open: open)
        if !stale.isEmpty {
            center.removeDeliveredNotifications(withIdentifiers: stale)
        }
        let pending = await center.pendingNotificationRequests().map { request in
            // The minute the request rings, so a changed `date_only_at` is
            // replaced even though the identifier stays.
            let moment = (request.trigger as? UNCalendarNotificationTrigger)?.dateComponents
            let date = moment.flatMap { parts in parts.year.flatMap { year in parts.month.flatMap { month in parts.day.map { VaultDay(year: year, month: month, day: $0) } } } }
            let time = moment.flatMap { parts in parts.hour.flatMap { hour in parts.minute.flatMap { ClockTime(hour: hour, minute: $0) } } }
            return ReminderSchedule.Pending(identifier: request.identifier, title: request.content.title, body: request.content.body, date: date, time: time)
        }
        let changes = ReminderSchedule.changes(pending: pending, wanted: alerts)
        if !changes.remove.isEmpty {
            center.removePendingNotificationRequests(withIdentifiers: changes.remove)
        }
        for alert in changes.add {
            let content = UNMutableNotificationContent()
            content.title = alert.title
            content.body = alert.body
            content.sound = .default
            content.categoryIdentifier = ReminderSchedule.category
            content.userInfo = ["reminder": alert.reminderID]
            // No time zone: the trigger is local wall-clock time wherever the
            // phone is, as reminders are (§4.1).
            let moment = DateComponents(year: alert.date.year, month: alert.date.month, day: alert.date.day, hour: alert.time.hour, minute: alert.time.minute)
            let trigger = UNCalendarNotificationTrigger(dateMatching: moment, repeats: false)
            do {
                try await center.add(UNNotificationRequest(identifier: alert.identifier, content: content, trigger: trigger))
            } catch {
                AppDelegate.model.show("A reminder couldn't be scheduled on this phone.")
            }
        }
    }

    /// Takes back every reminder notification, for a phone that disconnected.
    func clear() async {
        let ours = await center.pendingNotificationRequests().map(\.identifier).filter { $0.hasPrefix(ReminderSchedule.identifierPrefix) }
        center.removePendingNotificationRequests(withIdentifiers: ours)
    }

    // MARK: UNUserNotificationCenterDelegate

    /// A reminder that comes due while Thock is open still shows.
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification, withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner, .list, .sound])
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse, withCompletionHandler completionHandler: @escaping () -> Void) {
        let action = response.actionIdentifier
        let identifier = response.notification.request.identifier
        Task { @MainActor in
            guard let reminderID = ReminderSchedule.reminderID(in: identifier) else {
                completionHandler()
                return
            }
            if let answer = ReminderAnswer(rawValue: action) {
                await AppDelegate.model.answer(answer, reminderID: reminderID, due: ReminderSchedule.due(in: identifier))
            } else if action == UNNotificationDefaultActionIdentifier {
                AppDelegate.model.openRemindersFromNotification()
            }
            completionHandler()
        }
    }
}
