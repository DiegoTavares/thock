import Foundation

/// A reminder's state as a view shows it: the home line's (V45 §5.2), or,
/// when no line carries the id any more, what the index says happened.
public enum ReminderStatus: String, Hashable, Sendable {
    case open
    case closed
    case dropped
    /// The index stamped it `removed`: its line was deleted for good.
    case removed
    /// No line carries the id yet; the desk settles it (§6.2, *line not found*).
    case missing
}

/// One entry of the index, read through to its home line.
public struct IndexedReminder: Identifiable, Hashable, Sendable {
    public enum Section: String, Hashable, Sendable {
        case open
        case done
    }

    public enum Stamp: Hashable, Sendable {
        case done(VaultDay)
        case dropped(VaultDay)
        case removed(VaultDay)
    }

    public var id: String
    /// The heading the entry sits under. Bookkeeping only: the state always
    /// comes from the home line.
    public var section: Section
    /// The link's target (`projects/house`), or `nil` for a loose line that
    /// is its own home.
    public var link: String?
    public var stamp: Stamp?
    /// The entry's line in the index.
    public var entryLine: Int
    /// The home note's path.
    public var home: String
    /// The home line's index and text, when a line carries the id.
    public var line: Int?
    public var raw: String?
    public var reminder: LineReminder?
    public var status: ReminderStatus

    /// `reminderText` of the home line.
    public var text: String? {
        raw.map(Reminders.reminderText)
    }

    /// What a row shows: the text without a Done stamp a loose line carries.
    public var label: String {
        guard let text else { return "Line not found" }
        return Self.withoutStamp(text)
    }

    public var due: ReminderDue? { reminder?.due }

    /// The group a view lists it in on `today`: the home line's (§5.2), Done
    /// for a removed one, none while it is missing or has no day.
    public func group(today: VaultDay) -> ReminderGroup? {
        switch status {
        case .removed: return .done
        case .missing: return nil
        case .open, .closed, .dropped:
            guard let reminder else { return nil }
            return Reminders.group(state: reminder.state, due: reminder.due, today: today)
        }
    }

    static func withoutStamp(_ text: String) -> String {
        guard let range = text.range(of: #"\s*✅\s*\d{4}-\d{2}-\d{2}\s*$"#, options: .regularExpression) else { return text }
        return String(text[..<range.lowerBound])
    }
}

/// `reminders.md` and the notes it links to, read from the phone's copy the
/// way the desk reads them (V45 §6.2, §10.1): one reminder per index entry,
/// in file order, each found in its home note by its id.
public struct ReminderIndex: Equatable, Sendable {
    public var path: String
    public var reminders: [IndexedReminder]

    /// `content` reads a note from the vault, `nil` when it is missing.
    public init(config: VaultConfig, content: (String) -> String?) {
        let settings = config.reminders
        let readerConfig = config.reminderConfig
        path = settings.index
        let indexText = content(settings.index) ?? ""
        let file = TextFile(indexText)
        let headings = file.headings()
        func ranges(_ names: [String]) -> [Range<Int>] {
            headings.filter { heading in names.contains { Reminders.headingMatches(heading.text, $0) } }.map { heading in
                let section = file.section(of: heading)
                return section.start..<section.end
            }
        }
        let openRanges = ranges(settings.openHeadings)
        let doneRanges = ranges(settings.doneHeadings)

        var notes: [String: [ReminderAppearance]] = [:]
        var texts: [String: [String]] = [:]
        func appearances(in home: String) -> [ReminderAppearance]? {
            if let cached = notes[home] { return cached }
            guard let text = content(home) else { return nil }
            let found = Reminders.parseNote(text, path: home, config: readerConfig)
            notes[home] = found
            texts[home] = Self.lines(text)
            return found
        }

        var reminders: [IndexedReminder] = []
        for entry in Reminders.parseNote(indexText, path: settings.index, config: readerConfig) {
            guard let id = entry.reminder.id else { continue }
            let entryText = file.lines[entry.line].text
            let section: IndexedReminder.Section
            if openRanges.contains(where: { $0.contains(entry.line) }) {
                section = .open
            } else if doneRanges.contains(where: { $0.contains(entry.line) }) {
                section = .done
            } else {
                continue
            }
            let item = Bytes.trimStart(Array(entryText.utf8)[...])
            let afterMarker = Bytes.stripListMarker(item)
            guard afterMarker.count < item.count else { continue }
            let link = Self.linkTarget(Bytes.string(afterMarker))
            let home = link.map { $0 + ".md" } ?? settings.index
            var reminder = IndexedReminder(
                id: id, section: section, link: link, stamp: Self.stamp(entryText), entryLine: entry.line,
                home: home, line: nil, raw: nil, reminder: nil, status: .missing
            )
            let candidates = (appearances(in: home) ?? []).filter { !$0.mirror && $0.reminder.id == id }
            if let found = candidates.first(where: { $0.reminder.token != nil }) ?? candidates.first {
                reminder.line = found.line
                reminder.raw = texts[home]?[found.line]
                reminder.reminder = found.reminder
                switch found.reminder.state {
                case .open: reminder.status = .open
                case .closed: reminder.status = .closed
                case .dropped: reminder.status = .dropped
                }
            } else if case .removed? = reminder.stamp {
                reminder.status = .removed
            }
            reminders.append(reminder)
        }
        self.reminders = reminders
    }

    /// Every id the index holds, so a new one never collides (§4.3).
    public var ids: Set<String> {
        Set(reminders.map(\.id))
    }

    public func reminder(id: String) -> IndexedReminder? {
        reminders.first { $0.id == id }
    }

    /// The open reminders in `group`, soonest first; Done most recent first.
    public func reminders(in group: ReminderGroup, today: VaultDay, dateOnlyAt: ClockTime) -> [IndexedReminder] {
        let members = reminders.filter { $0.group(today: today) == group }
        guard group != .done else { return members.reversed() }
        return members.enumerated().sorted { left, right in
            let leftAt = left.element.due?.ringsAt(dateOnlyAt: dateOnlyAt)
            let rightAt = right.element.due?.ringsAt(dateOnlyAt: dateOnlyAt)
            if let leftAt, let rightAt, leftAt.date != rightAt.date || leftAt.time != rightAt.time {
                return (leftAt.date, leftAt.time.minutes) < (rightAt.date, rightAt.time.minutes)
            }
            return left.offset < right.offset
        }.map(\.element)
    }

    /// `[[target]]` or `[[target|alias]]` opening an entry.
    static func linkTarget(_ item: String) -> String? {
        guard item.hasPrefix("[["), let close = item.range(of: "]]") else { return nil }
        let inside = item[item.index(item.startIndex, offsetBy: 2)..<close.lowerBound]
        let target = inside.split(separator: "|", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? ""
        return target.isEmpty ? nil : target
    }

    /// `✅ <date>`, `dropped <date>` or `removed <date>` just before the
    /// entry's trailing comments.
    static func stamp(_ line: String) -> IndexedReminder.Stamp? {
        let body = NoteView.strippingTrailingComments(line)
        guard let range = body.range(of: #"(✅|dropped|removed) (\d{4}-\d{2}-\d{2})$"#, options: .regularExpression) else { return nil }
        let words = body[range].split(separator: " ")
        guard words.count == 2, let day = VaultDay(iso: String(words[1])), DateFormat.validDay(year: day.year, month: day.month, day: day.day) != nil else { return nil }
        switch words[0] {
        case "✅": return .done(day)
        case "dropped": return .dropped(day)
        default: return .removed(day)
        }
    }

    /// A note split on `\n`, a trailing `\r` belonging to the ending, the
    /// same lines `TextFile` reads.
    static func lines(_ text: String) -> [String] {
        TextFile(text).lines.map(\.text)
    }
}

// MARK: The chip (V45 §8.4)

/// How a token draws on a card or a row.
public enum ReminderChipState: Hashable, Sendable {
    case upcoming
    case today
    case overdue
    case closed
    /// A time-only token outside a daily note.
    case noDay
}

public struct ReminderChip: Hashable, Sendable {
    public var state: ReminderChipState
    public var label: String

    public init(state: ReminderChipState, label: String) {
        self.state = state
        self.label = label
    }

    /// The chip a line's token draws as on `today`, as the desk's
    /// `reminder_chip`: *Tomorrow 10:00*, *Today 14:30*, *Tue 20 Oct 18:00 ·
    /// overdue*, *Mon 19 Oct* when closed, *17:00 · which day?*.
    public init?(_ reminder: LineReminder, today: VaultDay) {
        guard let token = reminder.token else { return nil }
        guard let due = reminder.due else {
            state = .noDay
            if case .time(let time) = token.when {
                label = "\(time) · which day?"
            } else {
                label = "which day?"
            }
            return
        }
        switch Reminders.group(state: reminder.state, due: due, today: today) {
        case .done?, nil:
            state = .closed
            label = Self.dayLabel(due.date, today: today)
        case .overdue?:
            state = .overdue
            label = Self.dueLabel(due, today: today) + " · overdue"
        case .today?:
            state = .today
            label = Self.dueLabel(due, today: today)
        case .upcoming?:
            state = .upcoming
            label = Self.dueLabel(due, today: today)
        }
    }

    /// *Today*, *Tomorrow* or *Yesterday*, otherwise `ddd D MMM`, with the
    /// year when it is not `today`'s.
    public static func dayLabel(_ date: VaultDay, today: VaultDay) -> String {
        switch today.days(until: date) {
        case 0: return "Today"
        case 1: return "Tomorrow"
        case -1: return "Yesterday"
        default: return date.formatted(date.year == today.year ? "ddd D MMM" : "ddd D MMM YYYY")
        }
    }

    public static func dueLabel(_ due: ReminderDue, today: VaultDay) -> String {
        let day = dayLabel(due.date, today: today)
        return due.time.map { "\(day) \($0)" } ?? day
    }
}

// MARK: Writes (V45 §10.2–§10.4)

extension ReminderSettings {
    /// *In 1 hour*: `snoozeMinutes` after `now`, rounded up to the minute.
    public func snoozedDue(now: Date, calendar: Calendar) -> ReminderDue {
        let later = now.addingTimeInterval(TimeInterval(snoozeMinutes * 60))
        var parts = calendar.dateComponents([.year, .month, .day, .hour, .minute, .second, .nanosecond], from: later)
        if (parts.second ?? 0) != 0 || (parts.nanosecond ?? 0) != 0 {
            let rounded = calendar.date(byAdding: .minute, value: 1, to: later) ?? later
            parts = calendar.dateComponents([.year, .month, .day, .hour, .minute], from: rounded)
        }
        return ReminderDue(
            date: VaultDay(year: parts.year ?? 1970, month: parts.month ?? 1, day: parts.day ?? 1),
            time: ClockTime(clampingHour: parts.hour ?? 0, minute: parts.minute ?? 0)
        )
    }

    /// *Tomorrow 09:00*.
    public func tomorrowDue(today: VaultDay) -> ReminderDue {
        ReminderDue(date: today.adding(days: 1), time: tomorrowAt)
    }

    /// *Next week Mon 09:00*: the next Monday after `today`, never today.
    public func mondayDue(today: VaultDay) -> ReminderDue {
        // `weekday` is 1 for Sunday; days to the next Monday, 1 through 7.
        let ahead = (9 - today.weekday) % 7
        return ReminderDue(date: today.adding(days: ahead == 0 ? 7 : ahead), time: tomorrowAt)
    }
}

extension PhoneWrites {
    /// The line that makes a capture a reminder with no other home:
    /// `- [ ] <words> (@…) <!--rem:<id>-->`.
    func reminderLine(_ words: String, due: ReminderDue, id: String) -> String {
        let line = "- [ ] " + words.trimmingCharacters(in: .whitespaces)
        return Reminders.attach(line, due: due, id: id, homeDay: config.day(ofDailyPath: config.reminders.index)) ?? line
    }

    /// An append under the index's Open heading, which a missing index
    /// gets with the line (V45 §5.1).
    func indexAppend(_ lines: [String]) -> PlannedWrite {
        var write = document(.append, path: config.reminders.index)
        write.heading = HeadingRef(text: config.reminders.openHeadings.first ?? "Open", level: 2)
        write.lines = lines
        return PlannedWrite(document: write)
    }

    /// *Remind me…* on a line that is already in a note (§10.3): the token
    /// and a new id on the line, and a link to it in the index. A line that
    /// is already a reminder keeps its id, is rescheduled, and needs no new
    /// link. `nil` when the line cannot carry a reminder (a calendar line).
    public func remindMe(raw: String, path: String, heading: HeadingRef?, hash: String, ordinal: Int, due: ReminderDue, id: String) -> [PlannedWrite]? {
        guard let newLine = Reminders.attach(raw, due: due, id: id, homeDay: config.day(ofDailyPath: path)) else { return nil }
        var write = document(.replaceLine, path: path)
        write.heading = heading
        write.lineHash = hash
        write.ordinal = ordinal
        write.newLine = newLine
        let alreadyIndexed = Reminders.parseLine(raw, day: nil)?.id != nil
        guard !alreadyIndexed, path != config.reminders.index else { return [PlannedWrite(document: write)] }
        return [PlannedWrite(document: write), indexAppend([Reminders.indexLinkLine(path: path, id: id)])]
    }

    /// One edit of a reminder's home line, addressed in the whole file
    /// (`heading: null`) by its hash and ordinal, as rituals find it by id.
    func homeLineEdit(_ reminder: IndexedReminder, note: String, newLine: String?) -> PlannedWrite? {
        guard let line = reminder.line, let raw = reminder.raw, let newLine, newLine != raw else { return nil }
        let file = TextFile(note)
        let hash = SyncCore.lineHash(raw)
        let mask = file.contentMask()
        let twins = file.wholeFile().body.filter { mask[$0] && SyncCore.lineHash(file.lines[$0].text) == hash && !SyncCore.lineIdentity(file.lines[$0].text).isEmpty }
        guard let ordinal = twins.firstIndex(of: line) else { return nil }
        var write = document(.replaceLine, path: reminder.home)
        write.heading = nil
        write.lineHash = hash
        write.ordinal = ordinal
        write.newLine = newLine
        return PlannedWrite(document: write)
    }

    /// Ticks a task or strikes the token in prose.
    public func closeReminder(_ reminder: IndexedReminder, note: String) -> PlannedWrite? {
        homeLineEdit(reminder, note: note, newLine: reminder.raw.flatMap(Reminders.close))
    }

    public func reopenReminder(_ reminder: IndexedReminder, note: String) -> PlannedWrite? {
        homeLineEdit(reminder, note: note, newLine: reminder.raw.flatMap(Reminders.reopen))
    }

    /// Rewrites the token: the full date, unless the home is the daily note
    /// of the new day.
    public func rescheduleReminder(_ reminder: IndexedReminder, to due: ReminderDue, note: String) -> PlannedWrite? {
        let homeDay = config.day(ofDailyPath: reminder.home)
        return homeLineEdit(reminder, note: note, newLine: reminder.raw.flatMap { Reminders.rewrite($0, due: due, homeDay: homeDay) })
    }

    /// Removes the token and keeps the words and the id (V45 decision 16).
    public func dropReminder(_ reminder: IndexedReminder, note: String) -> PlannedWrite? {
        homeLineEdit(reminder, note: note, newLine: reminder.raw.flatMap(Reminders.dropToken))
    }
}

extension VaultSession {
    /// The index and its home lines, from the phone's copy. A vault without
    /// `reminders.md` has no reminders, never an error.
    public func reminderIndex() -> ReminderIndex {
        ReminderIndex(config: config, content: store.content)
    }

    /// A new id the index does not hold.
    func newReminderID() -> String {
        Reminders.newID(avoiding: reminderIndex().ids)
    }

    public func remindMe(_ item: PlannerItem, note: NoteID, due: ReminderDue, now: Date = Date()) throws {
        guard let writes = writes(now: now).remindMe(raw: item.raw, path: config.path(note), heading: item.heading, hash: item.hash, ordinal: item.ordinal, due: due, id: newReminderID()) else { return }
        try recordOnNote(writes, note: note, now: now)
    }

    public func remindMe(_ task: BacklogTask, due: ReminderDue, now: Date = Date()) throws {
        guard let writes = writes(now: now).remindMe(raw: task.raw, path: config.backlogFile, heading: task.heading, hash: task.hash, ordinal: task.ordinal, due: due, id: newReminderID()) else { return }
        try record(writes)
    }

    public func closeReminder(_ reminder: IndexedReminder, now: Date = Date()) throws {
        try editReminder(reminder, now: now) { builder, note in builder.closeReminder(reminder, note: note) }
    }

    public func reopenReminder(_ reminder: IndexedReminder, now: Date = Date()) throws {
        try editReminder(reminder, now: now) { builder, note in builder.reopenReminder(reminder, note: note) }
    }

    public func rescheduleReminder(_ reminder: IndexedReminder, to due: ReminderDue, now: Date = Date()) throws {
        try editReminder(reminder, now: now) { builder, note in builder.rescheduleReminder(reminder, to: due, note: note) }
    }

    public func dropReminder(_ reminder: IndexedReminder, now: Date = Date()) throws {
        try editReminder(reminder, now: now) { builder, note in builder.dropReminder(reminder, note: note) }
    }

    private func editReminder(_ reminder: IndexedReminder, now: Date = Date(), _ build: (PhoneWrites, String) -> PlannedWrite?) throws {
        guard let note = store.content(reminder.home), let write = build(writes(now: now), note) else { return }
        try record([write])
    }
}
