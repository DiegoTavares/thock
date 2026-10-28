import Foundation

// MARK: The phone reads (V48 §6.1)

extension VaultSession {
    /// The days from `days` before today to `days` after, as the watch draws
    /// them. `chip` is the phone's remembered chip for the watch's entry.
    public func watchSnapshot(days: Int = 7, chip: CaptureDestination = .today, now: Date = Date()) -> WatchSnapshot {
        let today = today(now: now)
        let state: WatchSnapshot.State
        if !store.isConnected {
            state = .notConnected
        } else if store.isReadOnly {
            state = .readOnly
        } else {
            state = .ready
        }
        let index = reminderIndex(now: now)
        let range = state == .notConnected ? [] : Array(-days...days)
        return WatchSnapshot(
            madeAt: now, state: state, today: today.iso,
            days: range.map { watchDay(today.adding(days: $0), today: today, index: index) },
            reminders: WatchSnapshot.Reminders(config.reminders), chip: chip.isRemembered ? chip : .today)
    }

    /// One day for a watch that asked for it outside the snapshot's range.
    public func watchDay(_ day: VaultDay, now: Date = Date()) -> WatchDay {
        watchDay(day, today: today(now: now), index: reminderIndex(now: now))
    }

    /// The planner as the phone's card draws it, in planner order, then the
    /// day's Reminders group. A day with no note has the reminders the index
    /// says are due on it.
    func watchDay(_ day: VaultDay, today: VaultDay, index: ReminderIndex) -> WatchDay {
        guard let text = noteText(day) else {
            return WatchDay(day: day.iso, exists: false, rows: Self.indexRows(index, day: day, today: today), notes: 0)
        }
        let planner = NoteView(text: text, config: config, day: day).planner
        guard planner.exists else { return WatchDay(day: day.iso, exists: true, rows: [], notes: 0) }
        let ordered = planner.groups.filter { !$0.isReminders } + planner.groups.filter(\.isReminders)
        let rows = ordered.flatMap { group in group.items.map { Self.row($0, day: day) } }
        return WatchDay(day: day.iso, exists: true, rows: rows, notes: planner.groups.map(\.notes.count).reduce(0, +))
    }

    static func row(_ item: PlannerItem, day: VaultDay) -> WatchRow {
        let isReminder = item.reminder != nil || item.mirror != nil
        let kind: WatchRow.Kind = item.isCalendar ? .calendar : isReminder ? .reminder : .task
        var start = item.time?.startMinutes
        // A reminder that rings on this day at a time sits at that time, even
        // when its line has no planner time of its own.
        if start == nil, let due = item.reminder?.due, due.date == day, let time = due.time {
            start = time.minutes
        }
        var carried: String?
        if let mirror = item.mirror, mirror.mark == nil, let from = mirror.carried {
            carried = from.date.iso
        }
        return WatchRow(hash: item.hash, ordinal: item.ordinal, kind: kind, start: start, end: start == nil ? nil : item.time?.endMinutes,
                        label: Inline.plainText(item.label), done: item.done || item.struck, carriedFrom: carried)
    }

    /// Reminders for a day the vault has no note for: those due on it, and
    /// on today those still open from earlier days, carried.
    static func indexRows(_ index: ReminderIndex, day: VaultDay, today: VaultDay) -> [WatchRow] {
        index.reminders.compactMap { reminder in
            guard let due = reminder.due, let raw = reminder.raw, reminder.status != .missing else { return nil }
            let carried = day == today && due.date < today && reminder.status == .open
            guard due.date == day || carried else { return nil }
            return WatchRow(hash: SyncCore.lineHash(raw), ordinal: 0, kind: .reminder, start: carried ? nil : due.time?.minutes,
                            label: Inline.plainText(reminder.label), done: reminder.status != .open, carriedFrom: carried ? due.date.iso : nil)
        }
        .sorted { ($0.start ?? Int.max) < ($1.start ?? Int.max) }
    }
}

// MARK: The phone writes (§6.3, §6.4)

extension VaultSession {
    public static let notReadyMessage = "Open Thock on your iPhone"
    public static let readOnlyMessage = "Thock Plus has ended. Captures are paused."
    public static let lineChangedMessage = "That line changed. Look again."
    public static let failedMessage = "That didn't save. Try again."

    /// How long a handled request id is remembered.
    static let watchRequestMemory: TimeInterval = 30 * 24 * 60 * 60

    /// Runs a watch request through the calls the phone's own screens make
    /// and answers it. A request already handled is answered as it was then
    /// and writes nothing. A snapshot or an answer is not a request: `nil`.
    public func receive(_ message: WatchMessage, now: Date = Date()) -> WatchMessage? {
        switch message {
        case .dayAsk(let ask):
            guard let day = VaultDay(iso: ask.day) else { return nil }
            return .dayReply(WatchDayReply(id: ask.id, day: watchDay(day, now: now)))
        case .capture, .tick:
            guard let id = message.requestID else { return nil }
            if let handled = store.watchRequest(id: id) { return .ack(handled) }
            let ack: WatchAck
            do {
                try perform(message)
                ack = WatchAck(id: id, outcome: .saved)
            } catch VaultSessionError.readOnly {
                ack = WatchAck(id: id, outcome: .refused, message: Self.readOnlyMessage)
            } catch VaultSessionError.lineChanged {
                ack = WatchAck(id: id, outcome: .refused, message: Self.lineChangedMessage)
            } catch {
                NSLog("Thock could not save a request from the watch: \(error)")
                ack = WatchAck(id: id, outcome: .refused, message: Self.failedMessage)
            }
            do {
                try store.recordWatchRequest(ack, at: now)
                try store.pruneWatchRequests(before: now.addingTimeInterval(-Self.watchRequestMemory))
            } catch {
                // The write stands; a resend would then be written twice,
                // which is better said than hidden.
                NSLog("Thock could not remember a watch request: \(error)")
            }
            return .ack(ack)
        case .snapshot, .ack, .dayReply:
            return nil
        }
    }

    private func perform(_ message: WatchMessage) throws {
        switch message {
        case .capture(let capture):
            // The watch's instant, so a capture delivered twice would make
            // the same digest and record as one delivered once.
            guard try self.capture(blocks: Blocks.parse(capture.text), destination: capture.destination, due: capture.reminderDue, now: capture.at) != nil else {
                throw WatchRequestError.nothingToSave
            }
        case .tick(let tick):
            guard let day = VaultDay(iso: tick.day) else { throw VaultSessionError.lineChanged }
            do {
                try self.tick(hash: tick.hash, ordinal: tick.ordinal, day: day)
            } catch VaultSessionError.lineChanged {
                try tickIndexedReminder(hash: tick.hash, day: day)
            }
        default:
            break
        }
    }

    /// A reminder row on a day with no note is its home line: a tick closes
    /// the reminder, as a tick on that line does on the phone.
    private func tickIndexedReminder(hash: String, day: VaultDay) throws {
        guard noteText(day) == nil,
              let reminder = reminderIndex().reminders.first(where: { $0.raw.map(SyncCore.lineHash) == hash && $0.status != .missing && $0.due != nil })
        else { throw VaultSessionError.lineChanged }
        guard reminder.status == .open else { return }
        try closeReminder(reminder)
    }
}

enum WatchRequestError: Error {
    case nothingToSave
}

// MARK: The timeline (§7.3)

/// What the Today complication draws at one moment.
public struct TimelineEntry: Hashable, Sendable {
    public var date: Date
    /// Minutes after midnight where the five-hour track starts.
    public var windowStart: Int
    /// The next timed row: the header's *14:00 Dentist*.
    public var next: WatchRow?
    /// Open tasks and reminders, timed or not: *3 left*.
    public var left: Int
    /// Open tasks and reminders with no time: *2 anytime*.
    public var anytime: Int

    public init(date: Date, windowStart: Int, next: WatchRow?, left: Int, anytime: Int) {
        self.date = date
        self.windowStart = windowStart
        self.next = next
        self.left = left
        self.anytime = anytime
    }
}

public enum TimelineEntries {
    public static let windowMinutes = 5 * 60
    public static let step = 15

    /// One entry now and one on every quarter hour after it to midnight, so
    /// the window and the header move without a reload.
    public static func entries(rows: [WatchRow], today: VaultDay, now: Date, calendar: Calendar = .current) -> [TimelineEntry] {
        guard let midnight = calendar.date(from: DateComponents(year: today.year, month: today.month, day: today.day)) else { return [] }
        let open = rows.filter { !$0.done && $0.kind != .calendar }
        let left = open.count
        let anytime = open.filter { $0.start == nil }.count
        var entries: [TimelineEntry] = []
        var minute = max(0, Int(now.timeIntervalSince(midnight) / 60))
        var date = now
        while minute < 24 * 60, entries.count < 96 {
            entries.append(TimelineEntry(date: date, windowStart: minute / 30 * 30, next: next(rows, at: minute), left: left, anytime: anytime))
            minute = (minute / step + 1) * step
            date = midnight.addingTimeInterval(TimeInterval(minute * 60))
        }
        return entries
    }

    /// The first timed row still ahead at `minute`: not done, and not a
    /// calendar event already started. A task or reminder stays next until
    /// it ends.
    public static func next(_ rows: [WatchRow], at minute: Int) -> WatchRow? {
        rows.filter { row in
            guard let start = row.start, !row.done else { return false }
            if row.kind == .calendar { return start >= minute }
            return (row.end ?? start) >= minute
        }
        .min { ($0.start ?? 0) < ($1.start ?? 0) }
    }
}
