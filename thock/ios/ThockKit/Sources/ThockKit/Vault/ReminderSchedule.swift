import Foundation

/// One local notification for a reminder (V45 §10.5).
public struct ReminderAlert: Hashable, Sendable {
    /// `rem:<id>:<due>`: a reminder rescheduled gets a new identifier, so a
    /// refresh replaces only what changed.
    public var identifier: String
    public var reminderID: String
    public var due: ReminderDue
    /// The local wall-clock minute it rings.
    public var date: VaultDay
    public var time: ClockTime
    /// The reminder's text, cut at 80 characters on a word.
    public var title: String
    /// `14:30 · projects/house`, or `today · reminders` for a date-only one.
    public var body: String
}

/// Which reminders the phone hands to iOS, and what changes when it looks
/// again. Pure, so the choice and the diff are tested here; only the
/// `UNUserNotificationCenter` calls live in the app.
public enum ReminderSchedule {
    /// iOS keeps at most 64 pending notifications per app.
    public static let limit = 64
    public static let identifierPrefix = "rem:"
    /// The notification category whose actions answer a reminder.
    public static let category = "thock.reminder"

    public static func identifier(id: String, due: ReminderDue) -> String {
        "\(identifierPrefix)\(id):\(due)"
    }

    /// The reminder id an identifier names, when it is one of ours.
    public static func reminderID(in identifier: String) -> String? {
        guard identifier.hasPrefix(identifierPrefix) else { return nil }
        let id = identifier.dropFirst(identifierPrefix.count).prefix(Reminders.idLength)
        return Reminders.isID(String(id)) ? String(id) : nil
    }

    /// The due an identifier was scheduled for, `nil` when it names none.
    public static func due(in identifier: String) -> ReminderDue? {
        guard let id = reminderID(in: identifier) else { return nil }
        let rest = identifier.dropFirst(identifierPrefix.count + id.count)
        guard rest.first == ":" else { return nil }
        return ReminderDue(String(rest.dropFirst()))
    }

    /// The identifier of every open reminder with a day, at its current due,
    /// whether it is still to ring or has rung.
    public static func openIdentifiers(index: ReminderIndex) -> Set<String> {
        Set(index.reminders.compactMap { reminder in
            guard reminder.status == .open, let due = reminder.due else { return nil }
            return identifier(id: reminder.id, due: due)
        })
    }

    /// Delivered reminder notifications that no longer stand for an open
    /// reminder at that due, to take off the lock screen. Others are never
    /// touched.
    public static func staleDelivered(_ delivered: [String], open: Set<String>) -> [String] {
        delivered.filter { $0.hasPrefix(identifierPrefix) && !open.contains($0) }
    }

    /// The soonest `limit` open reminders that ring after `now` (a day and a
    /// minute of local wall-clock time), soonest first.
    public static func alerts(index: ReminderIndex, settings: ReminderSettings, today: VaultDay, now: ClockTime, limit: Int = limit) -> [ReminderAlert] {
        var alerts: [ReminderAlert] = []
        for reminder in index.reminders where reminder.status == .open {
            guard let due = reminder.due else { continue }
            let moment = due.ringsAt(dateOnlyAt: settings.dateOnlyAt)
            guard (moment.date, moment.time.minutes) > (today, now.minutes) else { continue }
            // A date-only reminder rings on its own day, so it is "today" then.
            let when = due.time.map(\.description) ?? "today"
            alerts.append(ReminderAlert(
                identifier: identifier(id: reminder.id, due: due), reminderID: reminder.id, due: due,
                date: moment.date, time: moment.time,
                title: Reminders.cutText(Inline.plainText(reminder.label), maxCharacters: 80),
                body: "\(when) · \(Reminders.noteLink(reminder.home))"
            ))
        }
        let sorted = alerts.enumerated().sorted { left, right in
            let leftKey = (left.element.date, left.element.time.minutes)
            let rightKey = (right.element.date, right.element.time.minutes)
            return leftKey == rightKey ? left.offset < right.offset : leftKey < rightKey
        }
        return Array(sorted.prefix(limit).map(\.element))
    }

    /// A notification iOS holds: what it says and the minute it rings.
    public struct Pending: Equatable, Sendable {
        public var identifier: String
        public var title: String
        public var body: String
        public var date: VaultDay?
        public var time: ClockTime?

        public init(identifier: String, title: String, body: String, date: VaultDay?, time: ClockTime?) {
            self.identifier = identifier
            self.title = title
            self.body = body
            self.date = date
            self.time = time
        }
    }

    /// What to take back and what to add, given what iOS holds. Notifications
    /// that are not reminders are never touched; one that is still wanted is
    /// left alone, but its words and its minute are compared too, so a
    /// reworded reminder, or a date-only one whose hour changed, is replaced.
    public static func changes(pending: [Pending], wanted: [ReminderAlert]) -> (remove: [String], add: [ReminderAlert]) {
        let ours = pending.filter { $0.identifier.hasPrefix(identifierPrefix) }
        let wantedByIdentifier = Dictionary(wanted.map { ($0.identifier, $0) }, uniquingKeysWith: { first, _ in first })
        var kept: Set<String> = []
        var remove: [String] = []
        for request in ours {
            if let alert = wantedByIdentifier[request.identifier], alert.title == request.title, alert.body == request.body,
               alert.date == request.date, alert.time == request.time, !kept.contains(request.identifier) {
                kept.insert(request.identifier)
            } else {
                remove.append(request.identifier)
            }
        }
        return (remove, wanted.filter { !kept.contains($0.identifier) })
    }
}

/// A notification's three answers (V45 §10.5).
public enum ReminderAnswer: String, CaseIterable, Sendable {
    case done = "thock.reminder.done"
    case snooze = "thock.reminder.snooze"
    case tomorrow = "thock.reminder.tomorrow"
}

extension VaultSession {
    /// Answers a notification rung for `due` with one write on the home
    /// line: *Done* closes it, *In 1 hour* and *Tomorrow 09:00* rewrite its
    /// token. Returns false when there was nothing to write: the reminder is
    /// gone, closed elsewhere, or no longer due at `due` (the ring is stale).
    @discardableResult
    public func answer(_ answer: ReminderAnswer, reminderID: String, due: ReminderDue?, now: Date = Date()) throws -> Bool {
        guard let due, let reminder = reminderIndex(now: now).reminder(id: reminderID), reminder.status == .open,
              reminder.due == due, let note = store.content(reminder.home)
        else { return false }
        let builder = writes(now: now)
        let settings = config.reminders
        let write: PlannedWrite?
        switch answer {
        case .done: write = builder.closeReminder(reminder, note: note)
        case .snooze: write = builder.rescheduleReminder(reminder, to: settings.snoozedDue(now: now, calendar: calendar), note: note)
        case .tomorrow: write = builder.rescheduleReminder(reminder, to: settings.tomorrowDue(today: today(now: now)), note: note)
        }
        guard let write else { return false }
        try record([write])
        return true
    }

    /// Every open reminder's notification identifier, rung or not.
    public func openReminderIdentifiers(now: Date = Date()) -> Set<String> {
        ReminderSchedule.openIdentifiers(index: reminderIndex(now: now))
    }

    /// The alerts to hand iOS now.
    public func reminderAlerts(now: Date = Date()) -> [ReminderAlert] {
        let parts = calendar.dateComponents([.hour, .minute], from: now)
        return ReminderSchedule.alerts(index: reminderIndex(now: now), settings: config.reminders, today: today(now: now), now: ClockTime(clampingHour: parts.hour ?? 0, minute: parts.minute ?? 0))
    }
}
