import Foundation

/// A line found again by what it says, not where it sits (V46 §5.1): its
/// hash, and which of the lines with that hash under its heading it is.
/// The same identity a `replace_line` names its target by.
public struct LineKey: Codable, Equatable, Sendable {
    public var hash: String
    public var ordinal: Int

    public init(hash: String, ordinal: Int) {
        self.hash = hash
        self.ordinal = ordinal
    }
}

/// Where the person was when they left the app (V46 §5): the canvas, the
/// sheet over it, the line open in its editor, and what a capture held. It
/// is restored on the way back unless it is home.
public struct ResumePoint: Codable, Equatable, Sendable {
    public enum Canvas: Codable, Equatable, Sendable {
        case day(VaultDay)
        case week(VaultWeek)
        case backlog
        case reminders
    }

    public enum Sheet: Codable, Equatable, Sendable {
        case journal(day: VaultDay, entry: LineKey?)
        case section(note: NoteID, section: SectionKey, editing: LineKey?)
        case capture(entry: String, preset: CaptureDestination?)
        case clip
        case ask
        case receipts
        case you
        case calendar
    }

    /// What a capture or a clip held: nothing of it is in the vault until
    /// the person saves it (V46 §5.3).
    public struct Draft: Codable, Equatable, Sendable {
        public var markdown = ""
        public var destination: CaptureDestination?
        /// A reminder chip's time, as `ReminderDue.description` writes it.
        public var due: String?
        public var images: [ImageAttachment] = []
        public var link = ""
        public var why = ""

        public init() {}
    }

    public var canvas: Canvas
    public var sheet: Sheet?
    public var draft: Draft?
    /// The cursor's offset in the live editor.
    public var cursor: Int?
    /// The day the app was left on: the day canvas showing it is home.
    public var leftOn: VaultDay

    public init(canvas: Canvas, sheet: Sheet? = nil, draft: Draft? = nil, cursor: Int? = nil, leftOn: VaultDay) {
        self.canvas = canvas
        self.sheet = sheet
        self.draft = draft
        self.cursor = cursor
        self.leftOn = leftOn
    }

    /// Today's canvas with nothing over it (V46 §5.2). Coming back to it
    /// opens on the new today instead of restoring anything.
    public var isHome: Bool {
        sheet == nil && canvas == .day(leftOn)
    }

    /// Whether showing it reads the vault, so it waits for the owner. A
    /// capture or a clip shows only what the person typed (V33 §4.6).
    public var needsUnlock: Bool {
        switch sheet {
        case .capture?, .clip?: return false
        default: return true
        }
    }

    public static func load(from url: URL) -> ResumePoint? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder().decode(ResumePoint.self, from: data)
    }

    public func save(to url: URL) throws {
        try JSONEncoder().encode(self).write(to: url, options: [.atomic, .completeFileProtection])
    }
}

extension VaultSession {
    /// The identity of the line at `line` under `heading` in a note.
    public func lineKey(line: Int, heading: HeadingRef, note: NoteID) -> LineKey? {
        guard let text = noteText(note) else { return nil }
        let file = TextFile(text)
        guard file.lines.indices.contains(line) else { return nil }
        let hash = SyncCore.lineHash(file.lines[line].text)
        return LineKey(hash: hash, ordinal: writes().ordinal(of: line, hash: hash, in: text, heading: heading))
    }

    /// Where a line named by its identity sits now, or `nil` when the desk
    /// changed or removed it.
    public func line(for key: LineKey, heading: HeadingRef, note: NoteID) -> Int? {
        guard let text = noteText(note) else { return nil }
        let file = TextFile(text)
        guard let found = file.resolve(heading) else { return nil }
        let section = file.section(of: found)
        let mask = file.contentMask()
        let matches = section.body.filter { mask[$0] && SyncCore.lineHash(file.lines[$0].text) == key.hash }
        return matches.indices.contains(key.ordinal) ? matches[key.ordinal] : nil
    }
}
