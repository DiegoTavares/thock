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

    /// The note for a day as the canvas draws it. Today's note that does not
    /// exist yet is drawn from its template, so its sections are there to
    /// write into; the first write creates it.
    public func view(_ day: VaultDay, now: Date = Date()) -> NoteView? {
        let config = config
        if let text = store.content(config.dailyPath(day)) {
            return NoteView(text: text, config: config)
        }
        guard day == today(now: now), let seed = store.seedText(writes(now: now).dailySeed(day)) else { return nil }
        return NoteView(text: seed, config: config)
    }

    private func record(_ writes: [PlannedWrite]) throws {
        guard store.isConnected else { throw VaultSessionError.notConnected }
        guard !store.isReadOnly else { throw VaultSessionError.readOnly }
        try store.record(writes)
    }

    @discardableResult
    public func capture(blocks: [Block], destination: CaptureDestination, now: Date = Date()) throws -> CaptureRecord? {
        let builder = writes(now: now)
        let template = store.content(config.daily.template)
        guard let captured = builder.capture(blocks: blocks, destination: destination, todayNote: noteText(builder.today), template: template, taken: store.exists) else {
            return nil
        }
        try record(captured.writes)
        store.addCapture(captured.record)
        return captured.record
    }

    /// The waiting inbox note at `path` as the capture editor opens it, or
    /// `nil` when it is gone or cannot be edited from here.
    public func inboxEditorText(_ path: String) -> String? {
        store.content(path).flatMap(PhoneWrites.inboxEditorText)
    }

    /// Rewrites a waiting inbox note from the capture editor. Returns false
    /// when there was nothing to write.
    @discardableResult
    public func editInbox(path: String, blocks: [Block], now: Date = Date()) throws -> Bool {
        guard let note = store.content(path), let edit = writes(now: now).inboxEdit(path: path, note: note, blocks: blocks) else { return false }
        guard !edit.writes.isEmpty else { return false }
        try record(edit.writes)
        store.renameCapture(inboxPath: path, title: edit.title)
        return true
    }

    @discardableResult
    public func clip(_ clip: PhoneWrites.Clip, now: Date = Date()) throws -> CaptureRecord {
        let result = writes(now: now).clip(clip, taken: store.exists)
        try record(result.writes)
        store.addCapture(result.record)
        return result.record
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

    private func planner(_ day: VaultDay) -> Planner? {
        view(day)?.planner
    }

    public func tick(_ item: PlannerItem, day: VaultDay) throws {
        guard let planner = planner(day), let write = writes().tick(item, planner: planner, day: day) else { return }
        try record([write])
    }

    public func setTime(_ item: PlannerItem, startMinutes: Int?, endMinutes: Int?, day: VaultDay) throws {
        guard let planner = planner(day), let write = writes().setTime(item, startMinutes: startMinutes, endMinutes: endMinutes, planner: planner, day: day) else { return }
        try record([write])
    }

    public func editText(_ item: PlannerItem, text: String, day: VaultDay) throws {
        guard let planner = planner(day), let write = writes().editText(item, text: text, planner: planner, day: day) else { return }
        try record([write])
    }

    public func remove(_ item: PlannerItem, day: VaultDay) throws {
        guard let planner = planner(day) else { return }
        try record([writes().remove(item, planner: planner, day: day)])
    }

    public func moveToSoon(_ item: PlannerItem, day: VaultDay) throws {
        guard let planner = planner(day) else { return }
        try record(writes().moveToSoon(item, planner: planner, day: day))
    }

    public func addLine(_ text: String, group: PlannerGroup?, day: VaultDay) throws {
        guard let planner = planner(day), let write = writes().addLine(text, group: group, planner: planner, day: day) else { return }
        try record([write])
    }

    /// The next lines of today's plan that are still open, for the widgets.
    public func nextLines(limit: Int, now: Date = Date()) -> [PlannerItem] {
        guard let planner = planner(today(now: now)) else { return [] }
        let open = planner.items.filter { !$0.done && !$0.struck }
        let timed = open.filter { $0.time != nil }.sorted { ($0.time?.startMinutes ?? 0) < ($1.time?.startMinutes ?? 0) }
        let untimed = open.filter { $0.time == nil }
        return Array((timed + untimed).prefix(limit))
    }

    /// Ticks a line named by its hash, as a widget button does.
    public func tick(hash: String, ordinal: Int, day: VaultDay) throws {
        guard let item = planner(day)?.items.first(where: { $0.hash == hash && $0.ordinal == ordinal && !$0.isCalendar }) else { return }
        try tick(item, day: day)
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
