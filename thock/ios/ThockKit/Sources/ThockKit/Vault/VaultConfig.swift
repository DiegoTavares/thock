import Foundation

/// Just enough TOML to read the vault's own config files: tables, strings,
/// numbers, booleans, arrays and inline tables. Anything it cannot read is
/// skipped, so a config written by a newer desk never breaks the phone.
enum MiniTOML {
    indirect enum Value: Equatable {
        case string(String)
        case number(Double)
        case bool(Bool)
        case array([Value])
        case table([String: Value])

        var string: String? {
            if case .string(let value) = self { return value }
            return nil
        }
    }

    static func parse(_ text: String) -> [String: Value] {
        var values: [String: Value] = [:]
        var table = ""
        var skipping = false
        for rawLine in text.split(omittingEmptySubsequences: false, whereSeparator: \.isNewline) {
            let line = rawLine.trimmingCharacters(in: .whitespaces)
            if line.isEmpty || line.hasPrefix("#") { continue }
            if line.hasPrefix("[[") {
                // Arrays of tables (the Routine registry) are the desk's business.
                skipping = true
                continue
            }
            if line.hasPrefix("["), let close = line.firstIndex(of: "]") {
                table = line[line.index(after: line.startIndex)..<close].trimmingCharacters(in: .whitespaces)
                skipping = false
                continue
            }
            guard !skipping, let equals = line.firstIndex(of: "=") else { continue }
            let key = line[..<equals].trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "\"'"))
            var scanner = Scanner(Array(line[line.index(after: equals)...]))
            guard let value = scanner.value() else { continue }
            values[table.isEmpty ? key : table + "." + key] = value
        }
        return values
    }

    struct Scanner {
        var characters: [Character]
        var index = 0

        init(_ characters: [Character]) {
            self.characters = characters
        }

        mutating func skipSpaces() {
            while index < characters.count, characters[index].isWhitespace {
                index += 1
            }
        }

        mutating func value() -> Value? {
            skipSpaces()
            guard index < characters.count else { return nil }
            switch characters[index] {
            case "\"":
                index += 1
                var text = ""
                while index < characters.count, characters[index] != "\"" {
                    if characters[index] == "\\", index + 1 < characters.count {
                        index += 1
                        switch characters[index] {
                        case "n": text.append("\n")
                        case "t": text.append("\t")
                        default: text.append(characters[index])
                        }
                    } else {
                        text.append(characters[index])
                    }
                    index += 1
                }
                index += 1
                return .string(text)
            case "'":
                index += 1
                var text = ""
                while index < characters.count, characters[index] != "'" {
                    text.append(characters[index])
                    index += 1
                }
                index += 1
                return .string(text)
            case "[":
                index += 1
                var items: [Value] = []
                while true {
                    skipSpaces()
                    guard index < characters.count else { return .array(items) }
                    if characters[index] == "]" {
                        index += 1
                        return .array(items)
                    }
                    if characters[index] == "," {
                        index += 1
                        continue
                    }
                    guard let item = value() else { return .array(items) }
                    items.append(item)
                }
            case "{":
                index += 1
                var table: [String: Value] = [:]
                while true {
                    skipSpaces()
                    guard index < characters.count else { return .table(table) }
                    if characters[index] == "}" {
                        index += 1
                        return .table(table)
                    }
                    if characters[index] == "," {
                        index += 1
                        continue
                    }
                    var key = ""
                    while index < characters.count, characters[index] != "=", characters[index] != "}" {
                        key.append(characters[index])
                        index += 1
                    }
                    guard index < characters.count, characters[index] == "=" else { return .table(table) }
                    index += 1
                    guard let item = value() else { return .table(table) }
                    table[key.trimmingCharacters(in: .whitespaces)] = item
                }
            default:
                var token = ""
                while index < characters.count, !",]}#".contains(characters[index]), !characters[index].isWhitespace {
                    token.append(characters[index])
                    index += 1
                }
                if token == "true" { return .bool(true) }
                if token == "false" { return .bool(false) }
                if let number = Double(token) { return .number(number) }
                return nil
            }
        }
    }
}

public struct NotesConfig: Equatable, Sendable {
    public var dir: String
    public var filename: String
    public var template: String
}

/// What the phone reads from `.thock/config.toml` and `.thock/inbox.toml`.
/// It never writes either (V33 §14).
public struct VaultConfig: Equatable, Sendable {
    public static let configPath = ".thock/config.toml"
    public static let inboxConfigPath = ".thock/inbox.toml"
    public static let triageLogPath = "archives/inbox/triage-log.md"
    public static let clipsDir = "reference/clips"

    public var daily = NotesConfig(dir: "daily", filename: "YYYY-MM-DD", template: "templates/daily.md")
    public var weekly = NotesConfig(dir: "weekly", filename: "GGGG-[W]WW", template: "templates/weekly.md")
    public var backlogFile = "backlog.md"
    public var soonHeading = "Soon"
    public var somedayHeading = "Someday"
    public var completedHeading = "Completed"
    /// Canonical name first, aliases after (V26 §6.2).
    public var plannerHeadings = ["Day planner"]
    public var journalHeadings = ["Journal"]
    public var personalHeadings = ["Personal"]
    public var inboxDir = "inbox"

    public init() {}

    public init(config: String?, inboxConfig: String? = nil) {
        self.init()
        let values = MiniTOML.parse(config ?? "")
        func text(_ key: String) -> String? {
            guard let value = values[key]?.string?.trimmingCharacters(in: .whitespaces), !value.isEmpty else { return nil }
            return value
        }
        func folder(_ key: String) -> String? {
            text(key).map(Self.normalizedFolder)
        }
        daily.dir = folder("daily.dir") ?? daily.dir
        daily.filename = text("daily.filename") ?? daily.filename
        daily.template = text("daily.template") ?? daily.template
        weekly.dir = folder("weekly.dir") ?? weekly.dir
        weekly.filename = text("weekly.filename") ?? weekly.filename
        weekly.template = text("weekly.template") ?? weekly.template
        backlogFile = text("backlog.file") ?? backlogFile

        var headings: [String: MiniTOML.Value] = [:]
        if case .table(let inline)? = values["backlog.headings"] {
            headings = inline
        }
        func backlogHeading(_ key: String) -> String? {
            let value = headings[key]?.string ?? values["backlog.headings.\(key)"]?.string
            guard let trimmed = value?.trimmingCharacters(in: .whitespaces), !trimmed.isEmpty else { return nil }
            return trimmed
        }
        soonHeading = backlogHeading("soon") ?? soonHeading
        somedayHeading = backlogHeading("someday") ?? somedayHeading
        completedHeading = backlogHeading("completed") ?? completedHeading

        switch values["day_planner.heading"] {
        case .string(let name)?:
            let trimmed = name.trimmingCharacters(in: .whitespaces)
            if !trimmed.isEmpty { plannerHeadings = [trimmed] }
        case .array(let names)?:
            let cleaned = names.compactMap { $0.string?.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
            if !cleaned.isEmpty { plannerHeadings = cleaned }
        default:
            break
        }

        let inbox = MiniTOML.parse(inboxConfig ?? "")
        if let dir = inbox["dir"]?.string?.trimmingCharacters(in: CharacterSet(charactersIn: "/ ")), !dir.isEmpty {
            inboxDir = Self.normalizedFolder(dir)
        }
    }

    // Folders are joined with `/`, so `daily/` or `./daily` must not become
    // `daily//…` or `./daily/…`, which the service refuses as paths. `.`
    // becomes the empty string: the vault's root.
    static func normalizedFolder(_ value: String) -> String {
        value.split(separator: "/", omittingEmptySubsequences: true)
            .filter { $0 != "." }
            .joined(separator: "/")
    }

    /// Whether `path` is a note waiting directly in the inbox folder, as the
    /// desk counts them: top-level `.md` files only.
    public func isInboxNote(_ path: String) -> Bool {
        let prefix = inboxDir.isEmpty ? "" : inboxDir + "/"
        guard path.hasPrefix(prefix), path.hasSuffix(".md") else { return false }
        return !path.unicodeScalars.dropFirst(prefix.unicodeScalars.count).contains("/")
    }

    public func dailyPath(_ day: VaultDay) -> String {
        Self.join(daily.dir, "\(day.formatted(daily.filename)).md")
    }

    public func weeklyPath(_ day: VaultDay) -> String {
        Self.join(weekly.dir, "\(day.formatted(weekly.filename)).md")
    }

    /// `.` as a folder is the vault's root.
    static func join(_ folder: String, _ name: String) -> String {
        folder.isEmpty ? name : folder + "/" + name
    }
}

/// A calendar day with no time zone attached: the vault names notes by the
/// day the person lived, wherever the phone happened to be.
public struct VaultDay: Hashable, Comparable, Sendable, Codable {
    public var year: Int
    public var month: Int
    public var day: Int

    public init(year: Int, month: Int, day: Int) {
        self.year = year
        self.month = month
        self.day = day
    }

    public init(_ date: Date, calendar: Calendar = .current) {
        let parts = calendar.dateComponents([.year, .month, .day], from: date)
        self.init(year: parts.year ?? 1970, month: parts.month ?? 1, day: parts.day ?? 1)
    }

    public init?(iso: String) {
        let parts = iso.split(separator: "-").compactMap { Int($0) }
        guard parts.count == 3 else { return nil }
        self.init(year: parts[0], month: parts[1], day: parts[2])
    }

    public static func today() -> VaultDay {
        VaultDay(Date())
    }

    private static let utc: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC") ?? .gmt
        return calendar
    }()

    private static let isoWeek: Calendar = {
        var calendar = Calendar(identifier: .iso8601)
        calendar.timeZone = TimeZone(identifier: "UTC") ?? .gmt
        return calendar
    }()

    var utcDate: Date {
        Self.utc.date(from: DateComponents(year: year, month: month, day: day, hour: 12)) ?? Date(timeIntervalSince1970: 0)
    }

    public func adding(days: Int) -> VaultDay {
        VaultDay(Self.utc.date(byAdding: .day, value: days, to: utcDate) ?? utcDate, calendar: Self.utc)
    }

    public func days(until other: VaultDay) -> Int {
        Self.utc.dateComponents([.day], from: utcDate, to: other.utcDate).day ?? 0
    }

    /// 1 for Sunday through 7 for Saturday.
    public var weekday: Int {
        Self.utc.component(.weekday, from: utcDate)
    }

    public var isoWeek: Int {
        Self.isoWeek.component(.weekOfYear, from: utcDate)
    }

    public var isoWeekYear: Int {
        Self.isoWeek.component(.yearForWeekOfYear, from: utcDate)
    }

    public var iso: String {
        String(format: "%04d-%02d-%02d", year, month, day)
    }

    public static func < (left: VaultDay, right: VaultDay) -> Bool {
        (left.year, left.month, left.day) < (right.year, right.month, right.day)
    }

    static let monthNames = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"]
    static let weekdayNames = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"]

    public var monthName: String { Self.monthNames[min(max(month, 1), 12) - 1] }
    public var weekdayName: String { Self.weekdayNames[min(max(weekday, 1), 7) - 1] }

    /// The desk's moment-style vocabulary (`notes::format_date`): `YYYY YY
    /// MMMM MMM MM M DD D dddd ddd dd d WW W GGGG GG`, `[literal]` text, and
    /// everything else passed through.
    public func formatted(_ format: String) -> String {
        let characters = Array(format)
        var output = ""
        var index = 0
        while index < characters.count {
            let character = characters[index]
            if character == "[" {
                index += 1
                while index < characters.count, characters[index] != "]" {
                    output.append(characters[index])
                    index += 1
                }
                index += 1
                continue
            }
            guard "YMDdWG".contains(character) else {
                output.append(character)
                index += 1
                continue
            }
            var run = 1
            while index + run < characters.count, characters[index + run] == character {
                run += 1
            }
            output += token(character, run)
            index += run
        }
        return output
    }

    private func token(_ token: Character, _ run: Int) -> String {
        switch (token, run) {
        case ("Y", 2): return String(format: "%02d", year % 100)
        case ("Y", _): return String(format: "%04d", year)
        case ("M", 1): return String(month)
        case ("M", 2): return String(format: "%02d", month)
        case ("M", 3): return String(monthName.prefix(3))
        case ("M", _): return monthName
        case ("D", 1): return String(day)
        case ("D", _): return String(format: "%02d", day)
        case ("d", 1): return String(weekday - 1)
        case ("d", 2): return String(weekdayName.prefix(2))
        case ("d", 3): return String(weekdayName.prefix(3))
        case ("d", _): return weekdayName
        case ("W", 1): return String(isoWeek)
        case ("W", _): return String(format: "%02d", isoWeek)
        case ("G", 2): return String(format: "%02d", isoWeekYear % 100)
        case ("G", _): return String(format: "%04d", isoWeekYear)
        default: return weekdayName
        }
    }
}

public enum Template {
    /// Expands `{{date}}`, `{{date:FORMAT}}`, `{{time}}` and `{{title}}` the
    /// way the desk's `notes::expand_template` does; unknown tokens stay.
    public static func expand(_ template: String, day: VaultDay, time: String, title: String) -> String {
        var output = ""
        var rest = Substring(template)
        while let open = rest.range(of: "{{") {
            guard let close = rest[open.upperBound...].range(of: "}}") else { break }
            let token = rest[open.upperBound..<close.lowerBound]
            output += rest[..<open.lowerBound]
            switch token {
            case "date": output += day.iso
            case "time": output += time
            case "title": output += title
            default:
                if token.hasPrefix("date:") {
                    output += day.formatted(String(token.dropFirst(5)))
                } else {
                    output += rest[open.lowerBound..<close.upperBound]
                }
            }
            rest = rest[close.upperBound...]
        }
        output += rest
        return output
    }

    /// What the desk ships as `templates/daily.md`, used only when the vault
    /// has no template of its own.
    public static let defaultDaily = """
    # {{date:dddd, MMMM D, YYYY}}

    _A page for today. Write a little or a lot; it's yours._

    ___

    ## Journal

    _What happened, what you noticed, how it went._

    ___

    ## Day planner

    _Timed lines land on the planner beside you, like `- [ ] 09:00 - 10:00 Deep work`._

    ___

    ## Personal

    _The people, the errands, the small good things._

    """

    public static let defaultWeekly = """
    # Week {{date:W}}, {{date:GGGG}}

    _Seven days, one page. Set a direction at the start, look back at the end._

    ___

    ## Goals

    _Two or three things that would make this a good week._

    ___

    ## Notes

    _Anything worth keeping that doesn't belong to a single day._

    ___

    ## Week review

    _How did it go? The **Week Review** ritual appends its take below yours._

    """
}

public enum Slug {
    /// Lowercase ASCII alphanumerics and dashes, at most 60 characters, the
    /// desk's `gmail::slug`.
    public static func make(_ text: String, fallback: String = "item") -> String {
        var output = ""
        var lastDash = true
        for character in text {
            if character.isASCII, character.isLetter || character.isNumber {
                output.append(Character(character.lowercased()))
                lastDash = false
            } else if !lastDash {
                output.append("-")
                lastDash = true
            }
            if output.count >= 60 { break }
        }
        let trimmed = output.trimmingCharacters(in: CharacterSet(charactersIn: "-"))
        return trimmed.isEmpty ? fallback : trimmed
    }

    /// The desk's `inbox::sanitize_title`: no marker forgery, no wikilink
    /// forgery, whitespace collapsed, `(untitled)` when nothing is left.
    public static func sanitizedTitle(_ raw: String) -> String {
        var title = raw.replacingOccurrences(of: "<!--", with: "")
        while title.contains("[[") {
            title = title.replacingOccurrences(of: "[[", with: "[ [")
        }
        while title.contains("]]") {
            title = title.replacingOccurrences(of: "]]", with: "] ]")
        }
        title = title.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        return title.isEmpty ? "(untitled)" : title
    }
}
