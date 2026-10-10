import Foundation

public enum VaultSessionError: Error, Equatable {
    /// Plus lapsed: the phone shows what it has and writes nothing.
    case readOnly
    case notConnected
}

/// Everything the app, the share sheet and the widgets do to the vault, in
/// one place: read the local copy, build the write, apply and queue it.
public struct VaultSession: Sendable {
    public let store: VaultStore
    public var calendar: Calendar

    public init(store: VaultStore, calendar: Calendar = .current) {
        self.store = store
        self.calendar = calendar
    }

    public var config: VaultConfig { store.config }

    public func writes(now: Date = Date()) -> PhoneWrites {
        PhoneWrites(config: config, deviceID: store.deviceID, now: now, calendar: calendar)
    }

    public func today(now: Date = Date()) -> VaultDay {
        VaultDay(now, calendar: calendar)
    }

    public func noteText(_ day: VaultDay) -> String? {
        store.content(config.dailyPath(day))
    }

    public func noteText(_ note: NoteID) -> String? {
        store.content(config.path(note))
    }

    /// The note for a day as the canvas draws it. Today's note that does not
    /// exist yet is drawn from its template, so its sections are there to
    /// write into; the first write creates it.
    public func view(_ day: VaultDay, now: Date = Date()) -> NoteView? {
        view(.day(day), now: now)
    }

    /// A day or a week as the canvas draws it. A week with no note yet is
    /// always drawn from its template, whichever week it is (V37 §11 #6);
    /// a day only when it is today.
    public func view(_ note: NoteID, now: Date = Date()) -> NoteView? {
        let config = config
        if let text = store.content(config.path(note)) {
            return NoteView(text: text, config: config, kind: note.kind)
        }
        if case .day(let day) = note, day != today(now: now) { return nil }
        guard let seed = store.seedText(writes(now: now).seed(for: note)) else { return nil }
        return NoteView(text: seed, config: config, kind: note.kind)
    }

    // MARK: The calendar

    /// The days of a month that have a note, read from the phone's copy.
    public func daysWithNotes(in month: VaultMonth) -> Set<VaultDay> {
        let config = config
        var days = Set<VaultDay>()
        var day = month.first
        while month.contains(day) {
            if store.exists(config.dailyPath(day)) { days.insert(day) }
            day = day.adding(days: 1)
        }
        return days
    }

    public func hasNote(_ week: VaultWeek) -> Bool {
        store.exists(config.weeklyPath(week))
    }

    /// The oldest day the phone's copy holds a note for. A day before it is
    /// one sync has not brought down, not one that was never written
    /// (V37 §11 #7). `nil` when nothing is known, or the vault names days
    /// in a way the phone cannot read back.
    public func oldestDay() -> VaultDay? {
        let config = config
        return store.paths(under: config.daily.dir.isEmpty ? nil : config.daily.dir)
            .compactMap(config.day(ofDailyPath:))
            .min()
    }

    func record(_ writes: [PlannedWrite]) throws {
        guard store.isConnected else { throw VaultSessionError.notConnected }
        guard !store.isReadOnly else { throw VaultSessionError.readOnly }
        try store.record(writes)
    }

    /// Records writes that name lines of a note. When the note was drawn
    /// from its template and is not in the vault yet, it is created from the
    /// template first: without that both ends would make a bare note holding
    /// only the heading the write names.
    private func record(_ writes: [PlannedWrite], on note: NoteID, now: Date = Date()) throws {
        guard !writes.isEmpty else { return }
        let builder = self.writes(now: now)
        if noteText(note) == nil, store.seedText(builder.seed(for: note)) != nil {
            try record([builder.noteFromTemplate(note)] + writes)
        } else {
            try record(writes)
        }
    }

    @discardableResult
    public func capture(blocks: [Block], destination: CaptureDestination, images: [ImageAttachment] = [], now: Date = Date()) throws -> CaptureRecord? {
        let builder = writes(now: now)
        let template = store.content(config.daily.template)
        guard let captured = builder.capture(blocks: blocks, destination: destination, images: images, todayNote: noteText(builder.today), template: template, taken: taken) else {
            return nil
        }
        try record(captured.writes)
        store.addCapture(captured.record)
        return captured.record
    }

    /// Pictures shared with no link (V39 §7.3): one inbox note.
    @discardableResult
    public func photoCapture(images: [ImageAttachment], text: String, now: Date = Date()) throws -> CaptureRecord? {
        guard let captured = writes(now: now).photoCapture(images: images, text: text, taken: taken) else { return nil }
        try record(captured.writes)
        store.addCapture(captured.record)
        return captured.record
    }

    /// Whether a path is already in use on this phone: a note in the store,
    /// or a picture it captured.
    private func taken(_ path: String) -> Bool {
        store.exists(path) || store.hasBlob(path)
    }

    /// The waiting inbox note at `path` as the capture editor opens it, or
    /// `nil` when it is gone or cannot be edited from here.
    public func inboxEditorText(_ path: String) -> String? {
        store.content(path).flatMap(PhoneWrites.inboxEditorText)
    }

    /// Rewrites a waiting inbox note from the capture editor. Returns false
    /// when there was nothing to write.
    @discardableResult
    public func editInbox(path: String, blocks: [Block], images: [ImageAttachment] = [], now: Date = Date()) throws -> Bool {
        guard let note = store.content(path), let edit = writes(now: now).inboxEdit(path: path, note: note, blocks: blocks, images: images, taken: taken) else { return false }
        guard !edit.writes.isEmpty else { return false }
        try record(edit.writes)
        store.renameCapture(inboxPath: path, title: edit.title)
        return true
    }

    /// One of the inbox screen's gestures (V40 §5): the task line where it
    /// belongs, the note into `archives/inbox/`, one line in the triage log.
    /// A note that is already gone writes nothing.
    public func triageInbox(path: String, gesture: InboxGesture, now: Date = Date()) throws {
        guard let note = store.content(path) else { return }
        let builder = writes(now: now)
        try record(builder.inboxGesture(gesture, path: path, note: note, todayNote: noteText(builder.today), template: store.content(config.daily.template)))
    }

    @discardableResult
    public func clip(_ clip: PhoneWrites.Clip, now: Date = Date()) throws -> CaptureRecord {
        let result = writes(now: now).clip(clip, taken: store.exists)
        try record(result.writes)
        store.addCapture(result.record)
        return result.record
    }

    /// Queues the agent's note for later. Returns false when there was
    /// nothing to write.
    @discardableResult
    public func remember(_ text: String, now: Date = Date()) throws -> Bool {
        guard let write = writes(now: now).memoryNote(text) else { return false }
        try record([write])
        return true
    }

    @discardableResult
    public func keep(question: String, answer: String, now: Date = Date()) throws -> Bool {
        let builder = writes(now: now)
        guard let write = builder.keptAnswer(question: question, answer: answer, todayNote: noteText(builder.today)) else { return false }
        try record([write])
        return true
    }

    static let journalKey = "journal.last"

    /// Appends a timestamped entry to today's Journal. Returns false when
    /// there was nothing to write.
    @discardableResult
    public func journalAppend(blocks: [Block], now: Date = Date()) throws -> Bool {
        let builder = writes(now: now)
        guard let view = view(builder.today, now: now), let write = builder.journalAppend(blocks: blocks, journal: view.journal) else { return false }
        try record([write])
        store.setMeta(Self.journalKey, "\(now.timeIntervalSince1970)|\(builder.today.iso)|\(builder.clock)")
        return true
    }

    public func journalReplace(entry: JournalEntry, newText: String, day: VaultDay, now: Date = Date()) throws {
        guard let note = noteText(day) else { return }
        let view = NoteView(text: note, config: config)
        guard let write = writes(now: now).journalReplace(entry: entry, newText: newText, day: day, note: note, journal: view.journal) else { return }
        try record([write])
        if let last = store.meta(Self.journalKey)?.split(separator: "|"), last.count == 3, String(last[2]) == entry.time {
            store.setMeta(Self.journalKey, "\(now.timeIntervalSince1970)|\(day.iso)|\(last[2])")
        }
    }

    /// The phone's own last entry, when it was written less than ten minutes
    /// ago: opening the journal again continues that paragraph instead of
    /// starting a new timestamp (V33 §9).
    public func journalEntryToContinue(now: Date = Date()) -> JournalEntry? {
        guard let parts = store.meta(Self.journalKey)?.split(separator: "|"), parts.count == 3,
              let at = Double(parts[0]), now.timeIntervalSince1970 - at < 600, now.timeIntervalSince1970 >= at
        else { return nil }
        let today = today(now: now)
        guard String(parts[1]) == today.iso, let view = view(today, now: now) else { return nil }
        return view.journal.written.last { $0.time == String(parts[2]) }
    }

    private func planner(_ note: NoteID) -> Planner? {
        view(note)?.planner
    }

    public func tick(_ item: PlannerItem, note: NoteID) throws {
        guard let write = writes().tick(item, note: note) else { return }
        try record([write], on: note)
    }

    public func setTime(_ item: PlannerItem, startMinutes: Int?, endMinutes: Int?, note: NoteID) throws {
        guard let write = writes().setTime(item, startMinutes: startMinutes, endMinutes: endMinutes, note: note) else { return }
        try record([write], on: note)
    }

    public func editText(_ item: PlannerItem, text: String, note: NoteID) throws {
        guard let write = writes().editText(item, text: text, note: note) else { return }
        try record([write], on: note)
    }

    public func remove(_ item: PlannerItem, note: NoteID) throws {
        try record([writes().remove(item, note: note)], on: note)
    }

    public func moveToSoon(_ item: PlannerItem, note: NoteID) throws {
        try record(writes().moveToSoon(item, note: note), on: note)
    }

    public func addLine(_ text: String, group: PlannerGroup?, note: NoteID) throws {
        guard let planner = planner(note), let write = writes().addLine(text, group: group, planner: planner, note: note) else { return }
        try record([write])
    }

    /// Replaces one paragraph of a prose section (V37 §6, §8). Returns false
    /// when there was nothing to change.
    @discardableResult
    public func replaceParagraph(_ block: Block, in card: NoteCard, with newText: String, note: NoteID, now: Date = Date()) throws -> Bool {
        guard let text = noteText(note), let heading = card.heading else { return false }
        guard let write = writes(now: now).replaceParagraph(block, with: newText, heading: heading, note: note, text: text) else { return false }
        try record([write])
        return true
    }

    /// Adds a paragraph at the end of a prose section, creating the note from
    /// its template when the section was only drawn from it. Returns false
    /// when there was nothing to write.
    @discardableResult
    public func appendParagraph(blocks: [Block], to card: NoteCard, note: NoteID, now: Date = Date()) throws -> Bool {
        guard let heading = card.heading, let write = writes(now: now).appendParagraph(blocks: blocks, heading: heading, note: note) else { return false }
        try record([write])
        return true
    }

    /// The next lines of today's plan that are still open, for the widgets.
    public func nextLines(limit: Int, now: Date = Date()) -> [PlannerItem] {
        guard let planner = planner(.day(today(now: now))) else { return [] }
        let open = planner.items.filter { !$0.done && !$0.struck }
        let timed = open.filter { $0.time != nil }.sorted { ($0.time?.startMinutes ?? 0) < ($1.time?.startMinutes ?? 0) }
        let untimed = open.filter { $0.time == nil }
        return Array((timed + untimed).prefix(limit))
    }

    /// Ticks a line named by its hash, as a widget button does.
    public func tick(hash: String, ordinal: Int, day: VaultDay) throws {
        guard let item = planner(.day(day))?.items.first(where: { $0.hash == hash && $0.ordinal == ordinal && !$0.isCalendar }) else { return }
        try tick(item, note: .day(day))
    }

    /// Note names for `[[` autocomplete, best matches first.
    public func noteTitles(matching query: String, limit: Int = 12) -> [String] {
        let needle = query.lowercased()
        let stems = store.paths()
            .filter { $0.hasSuffix(".md") && !$0.hasPrefix(".") && !$0.hasPrefix("templates/") }
            .map { String($0.dropLast(3)) }
        let scored: [(String, Int)] = stems.compactMap { stem in
            let name = stem.split(separator: "/").last.map(String.init) ?? stem
            if needle.isEmpty { return (stem, stem.hasPrefix("daily/") || stem.hasPrefix("inbox/") ? 2 : 1) }
            if name.lowercased().hasPrefix(needle) { return (stem, 0) }
            if stem.lowercased().contains(needle) { return (stem, 1) }
            return nil
        }
        return scored.sorted { ($0.1, $0.0) < ($1.1, $1.0) }.prefix(limit).map { stem, _ in
            // The desk resolves a bare name when it is unique; otherwise the path.
            let name = stem.split(separator: "/").last.map(String.init) ?? stem
            return stems.filter { $0.hasSuffix("/" + name) || $0 == name }.count == 1 ? name : stem
        }
    }
}
