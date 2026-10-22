import Foundation

// Reminders (V45 §4, §5.2, §7.1, §11): the `(@…)` token and `<!--rem:…-->`
// id grammar, the daily-path day rule, where a token counts, mirror lines,
// state, and the home-line edits every writer makes. A port of the desk's
// `thock_sync_core::reminder`, held to the same vectors, so a line edited on
// either device comes out byte for byte the same.
//
// The grammar is read over UTF-8 bytes, as the desk reads it: Swift's
// `String` compares by grapheme and canonical equivalence, which would let a
// combining mark change what a line says. Token ranges are UTF-8 offsets.

/// A wall-clock time with minutes, the only precision a reminder has.
public struct ClockTime: Hashable, Comparable, Sendable, CustomStringConvertible {
    public var hour: Int
    public var minute: Int

    public init?(hour: Int, minute: Int) {
        guard (0...23).contains(hour), (0...59).contains(minute) else { return nil }
        self.hour = hour
        self.minute = minute
    }

    /// `hour:minute` held to 00:00–23:59.
    public init(clampingHour hour: Int, minute: Int) {
        self.hour = min(max(hour, 0), 23)
        self.minute = min(max(minute, 0), 59)
    }

    /// `H:MM` or `HH:MM`, 00:00 to 23:59.
    public init?(_ text: String) {
        guard let time = Reminders.parseClock(Array(text.utf8)[...]) else { return nil }
        self = time
    }

    public var minutes: Int { hour * 60 + minute }

    public var description: String { String(format: "%02d:%02d", hour, minute) }

    public static func < (left: ClockTime, right: ClockTime) -> Bool {
        left.minutes < right.minutes
    }
}

/// When a reminder is due: a day, and a time unless it is date-only.
public struct ReminderDue: Hashable, Comparable, Sendable, CustomStringConvertible {
    public var date: VaultDay
    public var time: ClockTime?

    public init(date: VaultDay, time: ClockTime?) {
        self.date = date
        self.time = time
    }

    /// Reads `YYYY-MM-DD` or `YYYY-MM-DD HH:MM` (`H:MM` too), the form
    /// `description` writes.
    public init?(_ text: String) {
        let bytes = Array(text.utf8)[...]
        if let space = bytes.firstIndex(of: UInt8(ascii: " ")) {
            guard let date = Reminders.parseISODate(bytes[..<space]), let time = Reminders.parseClock(bytes[(space + 1)...]) else { return nil }
            self.init(date: date, time: time)
        } else {
            guard let date = Reminders.parseISODate(bytes) else { return nil }
            self.init(date: date, time: nil)
        }
    }

    /// The local wall-clock moment it rings: its time, or `dateOnlyAt` on its
    /// day when it has none.
    public func ringsAt(dateOnlyAt: ClockTime) -> (date: VaultDay, time: ClockTime) {
        (date, time ?? dateOnlyAt)
    }

    public var description: String {
        guard let time else { return date.iso }
        return "\(date.iso) \(time)"
    }

    public static func < (left: ReminderDue, right: ReminderDue) -> Bool {
        if left.date != right.date { return left.date < right.date }
        switch (left.time, right.time) {
        case (nil, nil): return false
        case (nil, _): return true
        case (_, nil): return false
        case let (leftTime?, rightTime?): return leftTime < rightTime
        }
    }
}

/// What a token says, as written (§4.1).
public enum ReminderWhen: Hashable, Sendable {
    case date(VaultDay)
    case dateTime(VaultDay, ClockTime)
    /// `(@HH:MM)`: a day only inside a daily note.
    case time(ClockTime)

    /// The due this token names in a note whose day is `day` (a daily note's,
    /// or `nil` elsewhere). A time-only token outside a daily note has none.
    public func due(day: VaultDay?) -> ReminderDue? {
        switch self {
        case .date(let date): return ReminderDue(date: date, time: nil)
        case .dateTime(let date, let time): return ReminderDue(date: date, time: time)
        case .time(let time): return day.map { ReminderDue(date: $0, time: time) }
        }
    }
}

/// The token a line carries: where it is and what it says.
public struct ReminderToken: Hashable, Sendable {
    /// The token's UTF-8 offsets in the line, including the `~~` around a
    /// struck one.
    public var range: Range<Int>
    public var when: ReminderWhen
    /// `~~(@…)~~`: closed in prose.
    public var struck: Bool

    /// The token's source text in `line`.
    public func text(in line: String) -> String {
        String(decoding: Array(line.utf8)[range], as: UTF8.self)
    }
}

/// A task line's checkbox.
public enum ReminderTask: Hashable, Sendable {
    case open
    case done
}

/// A reminder's state, read from its home line alone (§5.2).
public enum ReminderState: String, Hashable, Sendable {
    /// An unticked task or prose with an unstruck token.
    case open
    /// A ticked task, or a struck token.
    case closed
    /// An id with no token left.
    case dropped
}

/// Where a view lists a reminder.
public enum ReminderGroup: String, Hashable, Comparable, Sendable, CaseIterable {
    case overdue
    case today
    case upcoming
    case done

    public static func < (left: ReminderGroup, right: ReminderGroup) -> Bool {
        allCases.firstIndex(of: left) ?? 0 < allCases.firstIndex(of: right) ?? 0
    }
}

/// What one line says about a reminder: its id, its counting token, and
/// whether it is a task.
public struct LineReminder: Hashable, Sendable {
    public var id: String?
    public var token: ReminderToken?
    /// `nil` for prose.
    public var task: ReminderTask?
    /// The token's due in this note; `nil` with no token, or for a
    /// time-only token outside a daily note (drawn as *which day?*).
    public var due: ReminderDue?

    public var state: ReminderState {
        guard let token else { return .dropped }
        return token.struck || task == .done ? .closed : .open
    }
}

/// One line of a note that carries a reminder token or id.
public struct ReminderAppearance: Hashable, Sendable {
    /// The line's index in the note, counting from 0.
    public var line: Int
    public var reminder: LineReminder
    /// A mirror line in a daily note's day section (§7.1): a copy that can
    /// never be a reminder's home.
    public var mirror: Bool
}

/// A vault's `[daily]` notes: the folder and the moment-style filename format.
public struct DailyNotes: Hashable, Sendable {
    public var dir: String
    public var filename: String

    public init(dir: String = "daily", filename: String = "YYYY-MM-DD") {
        self.dir = dir
        self.filename = filename
    }

    /// The day of the daily note at the vault-relative `path`, or `nil` when
    /// it is not one (§4.2). The path below `dir`, without `.md`, is read back
    /// through `filename`; a format with no year, month or day never resolves.
    public func day(of path: String) -> VaultDay? {
        guard DateFormat.namesADay(filename) else { return nil }
        var dir = Array(dir.utf8)[...]
        while dir.first == UInt8(ascii: "/") { dir = dir.dropFirst() }
        while dir.last == UInt8(ascii: "/") { dir = dir.dropLast() }
        var below = Array(path.utf8)[...]
        if !dir.isEmpty {
            guard below.starts(with: dir), below.dropFirst(dir.count).first == UInt8(ascii: "/") else { return nil }
            below = below.dropFirst(dir.count + 1)
        }
        guard Bytes.hasSuffix(below, ".md") else { return nil }
        return DateFormat.parse(Bytes.string(below.dropLast(3)), format: filename)
    }
}

/// What a reader needs to know about the vault to read reminders from a note.
public struct ReminderConfig: Hashable, Sendable {
    public var daily: DailyNotes
    /// The day section's heading names, matched loosely (V26). The first is
    /// the one written.
    public var dayHeadings: [String]

    public init(daily: DailyNotes = DailyNotes(), dayHeadings: [String] = [Reminders.defaultDayHeading]) {
        self.daily = daily
        self.dayHeadings = dayHeadings
    }
}

/// What a struck mirror line says happened to its reminder.
public enum MirrorMark: Hashable, Sendable {
    /// `~~text~~ → YYYY-MM-DD`: moved to another day.
    case moved(VaultDay)
    /// `~~text~~ (dropped)`.
    case dropped
    /// `~~text~~ (removed)`.
    case removed
}

/// A line in a daily note's day section:
/// `- [ ] [HH:MM ]<body>[ · <carried due>] · [[<note>]] <!--rem:<id>-->`,
/// where the body is the text, or `~~text~~` followed by its mark.
public struct MirrorLine: Hashable, Sendable, CustomStringConvertible {
    public var checked: Bool
    /// The planner time: the due's time for a reminder due that day.
    public var time: ClockTime?
    public var text: String
    public var mark: MirrorMark?
    /// The original due of a reminder carried from an earlier day.
    public var carried: ReminderDue?
    /// The home note's link target (`projects/house`).
    public var note: String
    public var id: String

    public init(checked: Bool, time: ClockTime?, text: String, mark: MirrorMark?, carried: ReminderDue?, note: String, id: String) {
        self.checked = checked
        self.time = time
        self.text = text
        self.mark = mark
        self.carried = carried
        self.note = note
        self.id = id
    }

    /// Reads a mirror line, or `nil` when the line does not have the shape
    /// (a person edited it, or it is not one).
    public init?(parsing line: String) {
        var rest = Bytes.trimEnd(Array(line.utf8)[...])
        let checked: Bool
        if Bytes.hasPrefix(rest, "- [ ] ") {
            checked = false
        } else if Bytes.hasPrefix(rest, "- [x] ") || Bytes.hasPrefix(rest, "- [X] ") {
            checked = true
        } else {
            return nil
        }
        rest = rest.dropFirst(6)
        guard let space = rest.lastIndex(of: UInt8(ascii: " ")),
              let id = Reminders.idInComment(rest[(space + 1)...])
        else { return nil }
        rest = rest[..<space]
        guard Bytes.hasSuffix(rest, "]]") else { return nil }
        rest = rest.dropLast(2)
        guard let link = Bytes.lastRange(of: Reminders.mirrorSeparator + "[[", in: rest) else { return nil }
        var note = rest[link.upperBound...]
        rest = rest[..<link.lowerBound]
        if let bar = note.firstIndex(of: UInt8(ascii: "|")) {
            note = note[..<bar]
        }
        guard !note.isEmpty, !note.contains(UInt8(ascii: "[")), !note.contains(UInt8(ascii: "]")) else { return nil }
        var carried: ReminderDue?
        if let separator = Bytes.lastRange(of: Reminders.mirrorSeparator, in: rest),
           let due = ReminderDue(Bytes.string(rest[separator.upperBound...]))
        {
            carried = due
            rest = rest[..<separator.lowerBound]
        }
        var time: ClockTime?
        var body = rest
        if let space = rest.firstIndex(of: UInt8(ascii: " ")) {
            let clock = rest[..<space]
            if clock.count == 5, let parsed = Reminders.parseClock(clock) {
                time = parsed
                body = rest[(space + 1)...]
            }
        }
        guard let (text, mark) = Self.parseBody(body) else { return nil }
        self.init(checked: checked, time: time, text: Bytes.string(text), mark: mark, carried: carried, note: Bytes.string(note), id: id)
    }

    private static func parseBody(_ body: ArraySlice<UInt8>) -> (ArraySlice<UInt8>, MirrorMark?)? {
        guard Bytes.hasPrefix(body, Reminders.strike) else {
            return body.isEmpty ? nil : (body, nil)
        }
        let struck = body.dropFirst(2)
        guard let close = Bytes.lastRange(of: Reminders.strike, in: struck) else { return nil }
        let text = struck[..<close.lowerBound]
        let after = Bytes.string(struck[close.upperBound...])
        if after == " (dropped)" { return (text, .dropped) }
        if after == " (removed)" { return (text, .removed) }
        guard after.hasPrefix(" → "),
              let date = Reminders.parseISODate(Array(after.utf8).dropFirst(" → ".utf8.count))
        else { return nil }
        return (text, .moved(date))
    }

    public var description: String {
        var line = "- [\(checked ? "x" : " ")] "
        if let time {
            line += "\(time) "
        }
        switch mark {
        case nil: line += text
        case .moved(let date)?: line += "~~\(text)~~ → \(date.iso)"
        case .dropped?: line += "~~\(text)~~ (dropped)"
        case .removed?: line += "~~\(text)~~ (removed)"
        }
        if let carried {
            line += Reminders.mirrorSeparator + carried.description
        }
        return line + Reminders.mirrorSeparator + "[[\(note)]] " + Reminders.idMarker(id)
    }
}

public enum Reminders {
    /// The number of lowercase hex digits in a reminder id.
    public static let idLength = 12
    /// The day section a daily note gets under its planner, unless
    /// `.thock/reminders.toml` names another.
    public static let defaultDayHeading = "Reminders"

    static let idOpen = "<!--rem:"
    static let commentOpen = "<!--"
    static let commentClose = "-->"
    static let calendarMarker = "<!--gcal:"
    static let tokenOpen = "(@"
    static let strike = "~~"
    static let mirrorSeparator = " · "

    /// Whether a token in the vault-relative `path` can be a reminder
    /// (§4.2): a Markdown file outside `.thock/`, `routines/` and the folder
    /// of each template in `templatePaths` (`templates/daily.md` excludes
    /// `templates/`; a template at the vault root excludes only itself).
    public static func isReminderPath(_ path: String, templatePaths: [String]) -> Bool {
        let bytes = Array(path.utf8)[...]
        guard let dot = bytes.lastIndex(of: UInt8(ascii: ".")), dot > bytes.startIndex,
              Bytes.string(bytes[(dot + 1)...]).lowercased() == "md",
              Bytes.string(bytes[(dot + 1)...]).utf8.count == 2
        else { return false }
        if Bytes.hasPrefix(bytes, ".thock/") || Bytes.hasPrefix(bytes, "routines/") {
            return false
        }
        return !templatePaths.contains { template in
            var trimmed = Array(template.utf8)[...]
            while trimmed.first == UInt8(ascii: "/") {
                trimmed = trimmed.dropFirst()
            }
            guard let slash = trimmed.lastIndex(of: UInt8(ascii: "/")) else {
                return bytes.elementsEqual(trimmed)
            }
            let folder = trimmed[..<slash]
            return bytes.starts(with: folder) && bytes.dropFirst(folder.count).first == UInt8(ascii: "/")
        }
    }

    /// The group a reminder belongs in on `today`: closed and dropped ones are
    /// Done; an open one is Overdue before its day, Today on it and Upcoming
    /// after. Groups go by day, not by instant. An open reminder with no day
    /// has none.
    public static func group(state: ReminderState, due: ReminderDue?, today: VaultDay) -> ReminderGroup? {
        guard state == .open else { return .done }
        guard let due else { return nil }
        if due.date < today { return .overdue }
        return due.date == today ? .today : .upcoming
    }

    /// Reads one line (§4.1–§4.3). `day` is the note's day when it is a daily
    /// note. `nil` when the line has neither a token nor an id, or carries a
    /// calendar marker. The line is read on its own: use `parseNote` for a
    /// whole file, which also skips fences, front matter and comments that
    /// span lines.
    public static func parseLine(_ line: String, day: VaultDay?) -> LineReminder? {
        readLine(Array(line.utf8)[...], day: day, inComment: false).reminder
    }

    /// Every reminder line in the note at the vault-relative `path`, top to
    /// bottom. Lines in fences, front matter and comments are text (§4.2).
    public static func parseNote(_ text: String, path: String, config: ReminderConfig) -> [ReminderAppearance] {
        let file = TextFile(text)
        let day = config.daily.day(of: path)
        let content = file.contentMask()
        let sections = day == nil ? [] : daySections(file, names: config.dayHeadings)
        var appearances: [ReminderAppearance] = []
        var inComment = false
        for (index, line) in file.lines.enumerated() where content[index] {
            let read = readLine(Array(line.text.utf8)[...], day: day, inComment: inComment)
            inComment = read.commentOpen
            guard let reminder = read.reminder else { continue }
            let mirror = reminder.token == nil && reminder.id != nil && sections.contains { $0.contains(index) }
            appearances.append(ReminderAppearance(line: index, reminder: reminder, mirror: mirror))
        }
        return appearances
    }

    static func daySections(_ file: TextFile, names: [String]) -> [Range<Int>] {
        let headings = file.headings()
        return headings
            .filter { heading in names.contains { headingMatches(heading.text, $0) } }
            .map { heading in
                let end = headings.first { $0.index > heading.index && $0.level <= heading.level }?.index ?? file.lines.count
                return heading.index + 1..<end
            }
    }

    /// Heading names match as the planner's do (V26): lowercase equality, or
    /// equal heading keys.
    static func headingMatches(_ text: String, _ name: String) -> Bool {
        if Bytes.trimmed(text).lowercased() == Bytes.trimmed(name).lowercased() {
            return true
        }
        let key = SyncCore.headingKey(name)
        return !key.isEmpty && SyncCore.headingKey(text) == key
    }

    private struct ReadLine {
        var reminder: LineReminder?
        /// An HTML comment opened on this line is still open at its end.
        var commentOpen: Bool
    }

    private static func readLine(_ line: ArraySlice<UInt8>, day: VaultDay?, inComment: Bool) -> ReadLine {
        let scan = scanInline(line, inComment: inComment)
        if Bytes.firstRange(of: calendarMarker, in: line) != nil {
            return ReadLine(reminder: nil, commentOpen: scan.commentOpen)
        }
        let token = lastToken(line, excluded: scan.excluded)
        guard token != nil || scan.id != nil else {
            return ReadLine(reminder: nil, commentOpen: scan.commentOpen)
        }
        let reminder = LineReminder(id: scan.id, token: token, task: taskOf(line)?.task, due: token?.when.due(day: day))
        return ReadLine(reminder: reminder, commentOpen: scan.commentOpen)
    }

    private struct InlineScan {
        /// Code spans and HTML comments, where a token is text.
        var excluded: [Range<Int>] = []
        /// The last reminder id among the line's comments.
        var id: String?
        var commentOpen = false
    }

    /// One pass over a line, left to right: a code span or an HTML comment,
    /// whichever opens first, hides what it holds (CommonMark's precedence).
    private static func scanInline(_ line: ArraySlice<UInt8>, inComment: Bool) -> InlineScan {
        let start = line.startIndex
        var scan = InlineScan()
        var cursor = start
        if inComment {
            guard let close = Bytes.firstRange(of: commentClose, in: line) else {
                return InlineScan(excluded: [0..<line.count], id: nil, commentOpen: true)
            }
            cursor = close.upperBound
            scan.excluded.append(0..<cursor - start)
        }
        while cursor < line.endIndex {
            let rest = line[cursor...]
            if Bytes.hasPrefix(rest, commentOpen) {
                let inside = rest.dropFirst(commentOpen.utf8.count)
                guard let close = Bytes.firstRange(of: commentClose, in: inside) else {
                    scan.excluded.append(cursor - start..<line.count)
                    scan.commentOpen = true
                    return scan
                }
                if let found = idInComment(line[cursor..<close.upperBound]) {
                    scan.id = found
                }
                scan.excluded.append(cursor - start..<close.upperBound - start)
                cursor = close.upperBound
            } else if line[cursor] == UInt8(ascii: "`") {
                let run = rest.prefix { $0 == UInt8(ascii: "`") }.count
                if let end = closingBackticks(line, from: cursor + run, run: run) {
                    scan.excluded.append(cursor - start..<end - start)
                    cursor = end
                } else {
                    cursor += run
                }
            } else {
                cursor += Bytes.scalarLength(line[cursor])
            }
        }
        return scan
    }

    /// The end of the backtick run of exactly `run` that closes a code span
    /// opened before `from`.
    private static func closingBackticks(_ line: ArraySlice<UInt8>, from: Int, run: Int) -> Int? {
        var search = from
        while search < line.endIndex, let start = line[search...].firstIndex(of: UInt8(ascii: "`")) {
            let length = line[start...].prefix { $0 == UInt8(ascii: "`") }.count
            if length == run {
                return start + length
            }
            search = start + length
        }
        return nil
    }

    static func idInComment(_ comment: ArraySlice<UInt8>) -> String? {
        guard Bytes.hasPrefix(comment, idOpen) else { return nil }
        let rest = comment.dropFirst(idOpen.utf8.count)
        guard Bytes.hasSuffix(rest, commentClose) else { return nil }
        let id = Bytes.string(rest.dropLast(commentClose.utf8.count))
        return isID(id) ? id : nil
    }

    /// Whether `text` is a reminder id: twelve lowercase hex digits.
    public static func isID(_ text: String) -> Bool {
        text.utf8.count == idLength && text.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }

    /// The id made of six random bytes, which the caller draws (and draws
    /// again when the index already holds it).
    public static func id(fromBytes bytes: [UInt8]) -> String {
        bytes.prefix(6).map { String(format: "%02x", $0) }.joined()
    }

    /// A fresh id that `taken` does not hold.
    public static func newID(avoiding taken: Set<String> = []) -> String {
        while true {
            var generator = SystemRandomNumberGenerator()
            let id = id(fromBytes: (0..<6).map { _ in UInt8.random(in: 0...255, using: &generator) })
            if !taken.contains(id) { return id }
        }
    }

    /// `<!--rem:<id>-->`.
    public static func idMarker(_ id: String) -> String {
        idOpen + id + commentClose
    }

    private static func lastToken(_ line: ArraySlice<UInt8>, excluded: [Range<Int>]) -> ReminderToken? {
        let base = line.startIndex
        var found: ReminderToken?
        var search = base
        while let open = Bytes.firstRange(of: tokenOpen, in: line[search...]) {
            let start = open.lowerBound
            search = open.upperBound
            if start > base, line[start - 1] == UInt8(ascii: "\\") {
                continue
            }
            guard let close = line[search...].firstIndex(of: UInt8(ascii: ")")) else { break }
            let end = close + 1
            if excluded.contains(where: { $0.lowerBound < end - base && start - base < $0.upperBound }) {
                continue
            }
            guard let when = parseWhen(line[search..<close]) else { continue }
            let struck = Bytes.hasSuffix(line[..<start], strike) && Bytes.hasPrefix(line[end...], strike)
            let range = struck ? start - 2 - base..<end + 2 - base : start - base..<end - base
            found = ReminderToken(range: range, when: when, struck: struck)
        }
        return found
    }

    private static func parseWhen(_ text: ArraySlice<UInt8>) -> ReminderWhen? {
        if let space = text.firstIndex(of: UInt8(ascii: " ")) {
            guard let date = parseISODate(text[..<space]), let time = parseClock(text[(space + 1)...]) else { return nil }
            return .dateTime(date, time)
        }
        if text.contains(UInt8(ascii: ":")) {
            return parseClock(text).map(ReminderWhen.time)
        }
        return parseISODate(text).map(ReminderWhen.date)
    }

    /// `YYYY-MM-DD`, a real calendar date.
    static func parseISODate(_ text: ArraySlice<UInt8>) -> VaultDay? {
        let bytes = Array(text)
        guard bytes.count == 10, bytes[4] == UInt8(ascii: "-"), bytes[7] == UInt8(ascii: "-"),
              bytes.enumerated().allSatisfy({ $0.offset == 4 || $0.offset == 7 || Bytes.isDigit($0.element) })
        else { return nil }
        return DateFormat.validDay(year: Bytes.number(bytes[0..<4]), month: Bytes.number(bytes[5..<7]), day: Bytes.number(bytes[8..<10]))
    }

    /// `H:MM` or `HH:MM`, 00:00 to 23:59.
    static func parseClock(_ text: ArraySlice<UInt8>) -> ClockTime? {
        guard let colon = text.firstIndex(of: UInt8(ascii: ":")) else { return nil }
        let hours = text[..<colon]
        let minutes = text[(colon + 1)...]
        guard (1...2).contains(hours.count), minutes.count == 2,
              hours.allSatisfy(Bytes.isDigit), minutes.allSatisfy(Bytes.isDigit)
        else { return nil }
        return ClockTime(hour: Bytes.number(hours), minute: Bytes.number(minutes))
    }

    /// A list item's checkbox and the UTF-8 offset of the character inside it.
    private static func taskOf(_ line: ArraySlice<UInt8>) -> (task: ReminderTask, mark: Int)? {
        let trimmed = Bytes.trimStart(line)
        let afterMarker = Bytes.stripListMarker(trimmed)
        guard afterMarker.count != trimmed.count else { return nil }
        let mark = line.count - afterMarker.count + 1
        let box = afterMarker.prefix(4)
        guard box.count == 4 else { return nil }
        switch Bytes.string(box) {
        case "[ ] ": return (.open, mark)
        case "[x] ", "[X] ": return (.done, mark)
        default: return nil
        }
    }

    /// The canonical token for `due` on a line whose note's day is `homeDay`:
    /// `(@HH:MM)` when the note is the daily note of the due's own day and the
    /// due has a time, the full date otherwise.
    public static func tokenText(_ due: ReminderDue, homeDay: VaultDay?) -> String {
        if let time = due.time, homeDay == due.date {
            return "\(tokenOpen)\(time))"
        }
        return "\(tokenOpen)\(due))"
    }

    // MARK: Home-line edits (§11, decision 21)
    //
    // Each returns `nil` when it does not apply to the line, so the caller
    // writes nothing.

    /// Closes a reminder: ticks a task, or strikes the token in prose.
    public static func close(_ line: String) -> String? {
        guard let reminder = parseLine(line, day: nil), reminder.state == .open else { return nil }
        let bytes = Array(line.utf8)
        if let (task, mark) = taskOf(bytes[...]), task == .open {
            return Bytes.replacing(bytes, mark..<mark + 1, with: "x")
        }
        guard let token = reminder.token else { return nil }
        let text = Bytes.string(bytes[token.range])
        return Bytes.replacing(bytes, token.range, with: strike + text + strike)
    }

    /// Reopens a closed reminder: unticks the task and unstrikes the token.
    public static func reopen(_ line: String) -> String? {
        guard let reminder = parseLine(line, day: nil), reminder.state == .closed, let token = reminder.token else { return nil }
        var edited = Array(line.utf8)
        if token.struck {
            let inner = edited[token.range.lowerBound + 2..<token.range.upperBound - 2]
            edited = Array(Bytes.replacing(edited, token.range, with: Bytes.string(inner)).utf8)
        }
        if let (task, mark) = taskOf(edited[...]), task == .done {
            edited = Array(Bytes.replacing(edited, mark..<mark + 1, with: " ").utf8)
        }
        return Bytes.string(edited[...])
    }

    /// Rewrites the token to `due` (`tokenText` decides the form), keeping a
    /// strike and the checkbox as they are. A dropped line gets the token
    /// back before its trailing comments.
    public static func rewrite(_ line: String, due: ReminderDue, homeDay: VaultDay?) -> String? {
        guard let reminder = parseLine(line, day: homeDay) else { return nil }
        let text = tokenText(due, homeDay: homeDay)
        let bytes = Array(line.utf8)
        switch reminder.token {
        case let token? where token.struck:
            return Bytes.replacing(bytes, token.range, with: strike + text + strike)
        case let token?:
            return Bytes.replacing(bytes, token.range, with: text)
        case nil:
            return insertBeforeTrailingComments(bytes[...], token: text)
        }
    }

    /// Drops a reminder: removes the token and keeps the words and the id.
    public static func dropToken(_ line: String) -> String? {
        guard let token = parseLine(line, day: nil)?.token else { return nil }
        return removeRange(Array(line.utf8)[...], token.range)
    }

    /// Makes `line` a reminder due at `due`: the token rewritten, or written
    /// before the trailing comments, and `<!--rem:<id>-->` last. A line that
    /// already has an id keeps it and `id` is not written.
    public static func attach(_ line: String, due: ReminderDue, id: String, homeDay: VaultDay?) -> String? {
        guard Bytes.firstRange(of: calendarMarker, in: Array(line.utf8)[...]) == nil, isID(id) else { return nil }
        let withToken: String
        if parseLine(line, day: homeDay) != nil {
            guard let rewritten = rewrite(line, due: due, homeDay: homeDay) else { return nil }
            withToken = rewritten
        } else {
            withToken = insertBeforeTrailingComments(Array(line.utf8)[...], token: tokenText(due, homeDay: homeDay))
        }
        return addID(withToken, id: id) ?? withToken
    }

    /// Appends `<!--rem:<id>-->` as the line's last comment. `nil` when the
    /// line already has an id or carries a calendar marker.
    public static func addID(_ line: String, id: String) -> String? {
        let bytes = Array(line.utf8)[...]
        guard Bytes.firstRange(of: calendarMarker, in: bytes) == nil, isID(id) else { return nil }
        if parseLine(line, day: nil)?.id != nil {
            return nil
        }
        let head = Bytes.trimEnd(bytes)
        return head.isEmpty ? idMarker(id) : Bytes.string(head) + " " + idMarker(id)
    }

    /// Removes `range` and one side's whitespace, so the words around it keep
    /// one space between them.
    private static func removeRange(_ line: ArraySlice<UInt8>, _ range: Range<Int>) -> String {
        let base = line.startIndex
        let before = line[..<(base + range.lowerBound)]
        let after = line[(base + range.upperBound)...]
        if Bytes.trimmed(after).isEmpty {
            return Bytes.string(Bytes.trimEnd(before))
        }
        if !Bytes.startsWithWhitespace(after) {
            return Bytes.string(Bytes.trimEnd(before)) + Bytes.string(after)
        }
        if before.isEmpty || Bytes.endsWithWhitespace(before) {
            return Bytes.string(before) + Bytes.string(Bytes.trimStart(after))
        }
        return Bytes.string(before) + Bytes.string(after)
    }

    /// The UTF-8 offset where the line's trailing run of HTML comments
    /// starts, or the end of its text.
    private static func trailingCommentsStart(_ line: ArraySlice<UInt8>) -> Int {
        var tail = Bytes.trimEnd(line)
        while Bytes.hasSuffix(tail, commentClose) {
            guard let open = Bytes.lastRange(of: commentOpen, in: tail)?.lowerBound else { break }
            if open + commentOpen.utf8.count > tail.endIndex - commentClose.utf8.count {
                break
            }
            tail = Bytes.trimEnd(tail[..<open])
        }
        return tail.endIndex - line.startIndex
    }

    private static func insertBeforeTrailingComments(_ line: ArraySlice<UInt8>, token: String) -> String {
        let split = line.startIndex + trailingCommentsStart(line)
        let head = line[..<split]
        let comments = Bytes.trimmed(line[split...])
        var edited = Bytes.string(head)
        if !head.isEmpty, head.last != UInt8(ascii: " ") {
            edited += " "
        }
        edited += token
        if !comments.isEmpty {
            edited += " " + Bytes.string(comments)
        }
        return edited
    }

    // MARK: Display text

    /// `text` without its counting token, the words around it kept one
    /// space apart, as `reminderText` removes it.
    public static func removingToken(_ text: String) -> String {
        guard let token = parseLine(text, day: nil)?.token else { return text }
        return removeRange(Array(text.utf8)[...], token.range)
    }

    /// The words a label shows for a reminder line: no list marker, checkbox,
    /// planner time, token or trailing comments (§4.3, §7.1).
    public static func reminderText(_ line: String) -> String {
        let withoutToken: [UInt8]
        if let token = parseLine(line, day: nil)?.token {
            withoutToken = Array(removeRange(Array(line.utf8)[...], token.range).utf8)
        } else {
            withoutToken = Array(line.utf8)
        }
        let text = withoutToken[..<trailingCommentsStart(withoutToken[...])]
        let trimmed = Bytes.trimStart(text)
        let item = Bytes.stripListMarker(trimmed)
        let words = item.count < trimmed.count ? Bytes.stripCheckbox(item) : trimmed
        let label = Bytes.string(words)
        let rest = TimePrefix.parse(label).map { String($0.rest) } ?? label
        return Bytes.string(Bytes.trimmed(Array(rest.utf8)[...]))
    }

    /// `text` cut to at most `maxCharacters` characters on a word boundary,
    /// with `…` when anything was cut. Characters are Unicode scalars, as on
    /// the desk.
    public static func cutText(_ text: String, maxCharacters: Int) -> String {
        let scalars = Array(text.unicodeScalars)
        guard scalars.count > maxCharacters else { return text }
        let prefix = scalars[..<maxCharacters]
        var cut = prefix
        if let space = prefix.lastIndex(where: { $0.properties.isWhitespace }), space > 0,
           !scalars[maxCharacters].properties.isWhitespace
        {
            cut = prefix[..<space]
        }
        while let last = cut.last, last.properties.isWhitespace {
            cut = cut.dropLast()
        }
        return String(String.UnicodeScalarView(cut)) + "…"
    }

    /// The wikilink target of a vault-relative note path: the path without `.md`.
    public static func noteLink(_ path: String) -> String {
        let bytes = Array(path.utf8)[...]
        return Bytes.hasSuffix(bytes, ".md") ? Bytes.string(bytes.dropLast(3)) : path
    }

    /// An index link line, `- [[<note>]] <!--rem:<id>-->`, for a reminder
    /// whose home is the note at `path` (§5.1, §10.3).
    public static func indexLinkLine(path: String, id: String) -> String {
        "- [[\(noteLink(path))]] \(idMarker(id))"
    }
}

/// The moment-style vocabulary of `[daily] filename` read backwards: the
/// inverse of `VaultDay.formatted`, as the desk's `dates::parse_date`.
public enum DateFormat {
    private enum Token {
        case literal(Unicode.Scalar)
        case run(Unicode.Scalar, Int)
    }

    private struct Fields {
        var year: Int?
        var month: Int?
        var day: Int?
        var isoYear: Int?
        var week: Int?

        /// A calendar date when the format carries one, otherwise the Monday
        /// of the ISO week it carries.
        func resolve() -> VaultDay? {
            if let year, let month, let day {
                return DateFormat.validDay(year: year, month: month, day: day)
            }
            guard let isoYear, let week, (1...53).contains(week) else { return nil }
            return VaultDay.isoWeekMonday(year: isoYear, week: week)
        }
    }

    static let monthNames = VaultDay.monthNames.map { Array($0.unicodeScalars) }
    static let weekdayNames = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"].map { Array($0.unicodeScalars) }

    /// The day that formats to exactly `text`, or `nil`. A format must carry
    /// year, month and day tokens, or ISO year and week tokens (the Monday of
    /// that week is returned); weekday tokens are consumed but say nothing.
    public static func parse(_ text: String, format: String) -> VaultDay? {
        match(tokens(format)[...], Array(text.unicodeScalars)[...], Fields(), format: format, text: text)
    }

    /// Whether `format` carries a year, a month and a day of the month, so a
    /// stem read back through it names one calendar day.
    public static func namesADay(_ format: String) -> Bool {
        let tokens = tokens(format)
        func has(_ wanted: Unicode.Scalar) -> Bool {
            tokens.contains { if case .run(wanted, _) = $0 { return true } else { return false } }
        }
        return has("Y") && has("M") && has("D")
    }

    static func validDay(year: Int, month: Int, day: Int) -> VaultDay? {
        guard (1...12).contains(month), day >= 1 else { return nil }
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
        let lengths = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        guard day <= lengths[month - 1] else { return nil }
        return VaultDay(year: year, month: month, day: day)
    }

    private static func tokens(_ format: String) -> [Token] {
        let characters = Array(format.unicodeScalars)
        var tokens: [Token] = []
        var index = 0
        while index < characters.count {
            let character = characters[index]
            if character == "[" {
                index += 1
                while index < characters.count, characters[index] != "]" {
                    tokens.append(.literal(characters[index]))
                    index += 1
                }
                index += 1
            } else if "YMDdWG".unicodeScalars.contains(character) {
                var run = 1
                while index + run < characters.count, characters[index + run] == character {
                    run += 1
                }
                tokens.append(.run(character, run))
                index += run
            } else {
                tokens.append(.literal(character))
                index += 1
            }
        }
        return tokens
    }

    /// Backtracking over the tokens. Variable-width numbers (`M`, `D`, `W`)
    /// try the longer width first; the closing roundtrip check rejects any
    /// loose match that does not give the input back.
    private static func match(_ tokens: ArraySlice<Token>, _ input: ArraySlice<Unicode.Scalar>, _ fields: Fields, format: String, text: String) -> VaultDay? {
        guard let token = tokens.first else {
            guard input.isEmpty, let day = fields.resolve() else { return nil }
            return day.formatted(format) == text ? day : nil
        }
        let rest = tokens.dropFirst()
        func next(_ input: ArraySlice<Unicode.Scalar>, _ fields: Fields) -> VaultDay? {
            match(rest, input, fields, format: format, text: text)
        }
        func number(_ width: Int, _ set: (inout Fields, Int) -> Void) -> VaultDay? {
            guard let (value, after) = digits(input, width) else { return nil }
            var updated = fields
            set(&updated, value)
            return next(after, updated)
        }
        func eitherWidth(_ set: @escaping (inout Fields, Int) -> Void) -> VaultDay? {
            number(2, set) ?? number(1, set)
        }
        switch token {
        case .literal(let literal):
            guard input.first == literal else { return nil }
            return next(input.dropFirst(), fields)
        case .run("Y", 2): return number(2) { $0.year = 2000 + $1 }
        case .run("Y", _): return number(4) { $0.year = $1 }
        case .run("M", 1): return eitherWidth { $0.month = $1 }
        case .run("M", 2): return number(2) { $0.month = $1 }
        case .run("M", let run):
            for (index, name) in monthNames.enumerated() {
                let name = run == 3 ? name.prefix(3) : name[...]
                guard input.starts(with: name) else { continue }
                var updated = fields
                updated.month = index + 1
                if let found = next(input.dropFirst(name.count), updated) { return found }
            }
            return nil
        case .run("D", 1): return eitherWidth { $0.day = $1 }
        case .run("D", _): return number(2) { $0.day = $1 }
        case .run("d", 1): return number(1) { _, _ in }
        case .run("d", let run):
            for name in weekdayNames {
                let name = run == 2 ? name.prefix(2) : run == 3 ? name.prefix(3) : name[...]
                guard input.starts(with: name) else { continue }
                if let found = next(input.dropFirst(name.count), fields) { return found }
            }
            return nil
        case .run("W", 1): return eitherWidth { $0.week = $1 }
        case .run("W", _): return number(2) { $0.week = $1 }
        case .run("G", 2): return number(2) { $0.isoYear = 2000 + $1 }
        case .run("G", _): return number(4) { $0.isoYear = $1 }
        case .run:
            return nil
        }
    }

    /// Exactly `width` ASCII digits from the start of `input`.
    private static func digits(_ input: ArraySlice<Unicode.Scalar>, _ width: Int) -> (Int, ArraySlice<Unicode.Scalar>)? {
        let head = input.prefix(width)
        guard head.count == width, head.allSatisfy({ ("0"..."9").contains($0) }) else { return nil }
        let value = head.reduce(0) { $0 * 10 + Int($1.value - 48) }
        return (value, input.dropFirst(width))
    }
}

/// Byte-level helpers that read as the desk's `str` methods do.
enum Bytes {
    static func string(_ bytes: ArraySlice<UInt8>) -> String {
        String(decoding: bytes, as: UTF8.self)
    }

    static func replacing(_ bytes: [UInt8], _ range: Range<Int>, with text: String) -> String {
        string(bytes[..<range.lowerBound]) + text + string(bytes[range.upperBound...])
    }

    static func isDigit(_ byte: UInt8) -> Bool {
        (48...57).contains(byte)
    }

    static func number<C: Collection>(_ digits: C) -> Int where C.Element == UInt8 {
        digits.reduce(0) { $0 * 10 + Int($1 - 48) }
    }

    static func hasPrefix(_ bytes: ArraySlice<UInt8>, _ prefix: String) -> Bool {
        bytes.starts(with: prefix.utf8)
    }

    static func hasSuffix(_ bytes: ArraySlice<UInt8>, _ suffix: String) -> Bool {
        bytes.reversed().starts(with: suffix.utf8.reversed())
    }

    static func firstRange(of needle: String, in bytes: ArraySlice<UInt8>) -> Range<Int>? {
        let pattern = Array(needle.utf8)
        guard !pattern.isEmpty, bytes.count >= pattern.count else { return nil }
        var index = bytes.startIndex
        while index <= bytes.endIndex - pattern.count {
            if bytes[index..<index + pattern.count].elementsEqual(pattern) {
                return index..<index + pattern.count
            }
            index += 1
        }
        return nil
    }

    static func lastRange(of needle: String, in bytes: ArraySlice<UInt8>) -> Range<Int>? {
        let pattern = Array(needle.utf8)
        guard !pattern.isEmpty, bytes.count >= pattern.count else { return nil }
        var index = bytes.endIndex - pattern.count
        while index >= bytes.startIndex {
            if bytes[index..<index + pattern.count].elementsEqual(pattern) {
                return index..<index + pattern.count
            }
            index -= 1
        }
        return nil
    }

    /// The length of the UTF-8 sequence a lead byte opens.
    static func scalarLength(_ lead: UInt8) -> Int {
        switch lead {
        case 0xF0...: return 4
        case 0xE0...: return 3
        case 0xC0...: return 2
        default: return 1
        }
    }

    /// The scalars of a slice that falls on scalar boundaries.
    private static func scalars(_ bytes: ArraySlice<UInt8>) -> [(scalar: Unicode.Scalar, length: Int)] {
        string(bytes).unicodeScalars.map { ($0, $0.utf8.count) }
    }

    static func trimStart(_ bytes: ArraySlice<UInt8>) -> ArraySlice<UInt8> {
        let leading = scalars(bytes).prefix { $0.scalar.properties.isWhitespace }.reduce(0) { $0 + $1.length }
        return bytes.dropFirst(leading)
    }

    static func trimEnd(_ bytes: ArraySlice<UInt8>) -> ArraySlice<UInt8> {
        let trailing = scalars(bytes).reversed().prefix { $0.scalar.properties.isWhitespace }.reduce(0) { $0 + $1.length }
        return bytes.dropLast(trailing)
    }

    static func trimmed(_ bytes: ArraySlice<UInt8>) -> ArraySlice<UInt8> {
        trimEnd(trimStart(bytes))
    }

    static func trimmed(_ text: String) -> String {
        string(trimmed(Array(text.utf8)[...]))
    }

    static func startsWithWhitespace(_ bytes: ArraySlice<UInt8>) -> Bool {
        string(bytes).unicodeScalars.first?.properties.isWhitespace ?? false
    }

    static func endsWithWhitespace(_ bytes: ArraySlice<UInt8>) -> Bool {
        string(bytes).unicodeScalars.last?.properties.isWhitespace ?? false
    }

    static func stripListMarker(_ text: ArraySlice<UInt8>) -> ArraySlice<UInt8> {
        for marker in ["- ", "* ", "+ "] where hasPrefix(text, marker) {
            return text.dropFirst(2)
        }
        let digits = text.prefix(while: isDigit).count
        if (1...9).contains(digits) {
            let after = text.dropFirst(digits)
            if hasPrefix(after, ". ") || hasPrefix(after, ") ") {
                return after.dropFirst(2)
            }
        }
        return text
    }

    static func stripCheckbox(_ text: ArraySlice<UInt8>) -> ArraySlice<UInt8> {
        for checkbox in ["[ ] ", "[x] ", "[X] "] where hasPrefix(text, checkbox) {
            return text.dropFirst(4)
        }
        return text
    }
}
