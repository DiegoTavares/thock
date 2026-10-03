import CryptoKit
import Foundation

/// The rules both ends of vault sync must agree on (V34 API §7–§9): heading
/// keys, line and section hashes, write application and the envelope. A Swift
/// port of the shared core, kept honest by the fixtures under
/// `Tests/ThockKitTests/Fixtures/v1` and, once they exist, by the desk's own
/// under `crates/thock-sync-core/fixtures/v1`.
public enum SyncCore {
    /// Marks the second of two versions of a line that were both kept.
    public static let conflictMarker = " <!--thock:also-->"

    static func sha256Hex(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    static func sha256Hex(_ text: String) -> String {
        sha256Hex(Data(text.utf8))
    }

    /// A heading's decoration-insensitive comparison key (V26 §5.1): links
    /// reduced to their label, every character that is not a letter or digit
    /// folded to a single space, lowercased and trimmed.
    public static func headingKey(_ text: String) -> String {
        var key = ""
        for scalar in reducingLinks(text).unicodeScalars {
            if isAlphanumeric(scalar) {
                key += scalar.properties.lowercaseMapping
            } else if !key.isEmpty, !key.hasSuffix(" ") {
                key += " "
            }
        }
        if key.hasSuffix(" ") {
            key.removeLast()
        }
        return key
    }

    private static func isAlphanumeric(_ scalar: Unicode.Scalar) -> Bool {
        if scalar.properties.isAlphabetic { return true }
        switch scalar.properties.generalCategory {
        case .decimalNumber, .letterNumber, .otherNumber: return true
        default: return false
        }
    }

    /// Replaces `[[target|alias]]`, `[[target]]` and `[label](destination)`
    /// with their label, skipping images, embeds, escapes, inline code and
    /// HTML comments, as the desk's `inline_links` does.
    static func reducingLinks(_ text: String) -> String {
        let characters = Array(text)
        let excluded = inlineExclusions(characters)
        var output = ""
        var index = 0
        while index < characters.count {
            let character = characters[index]
            guard character == "[" else {
                output.append(character)
                index += 1
                continue
            }
            if index > 0, characters[index - 1] == "!" || characters[index - 1] == "\\" {
                output.append(character)
                index += 1
                continue
            }
            if excluded.contains(where: { $0.contains(index) }) {
                output.append(character)
                index += 1
                continue
            }
            if let link = parseLink(characters, at: index, asTheDeskKeysIt: true), !excluded.contains(where: { $0.overlaps(link.range) }) {
                output.append(contentsOf: characters[link.label])
                index = link.range.upperBound
            } else {
                output.append(character)
                index += 1
            }
        }
        return output
    }

    struct ParsedLink {
        var range: Range<Int>
        var label: Range<Int>
        var wikiTarget: Range<Int>?
        var destination: Range<Int>?
    }

    /// `asTheDeskKeysIt` reads an inline link's destination the way the
    /// desk's `heading_key` does (spaces and balanced parentheses allowed, may
    /// be empty), so a heading resolves to the same key on both sides.
    static func parseLink(_ characters: [Character], at open: Int, asTheDeskKeysIt: Bool = false) -> ParsedLink? {
        if open + 1 < characters.count, characters[open + 1] == "[" {
            let innerStart = open + 2
            var close = innerStart
            while close + 1 < characters.count, !(characters[close] == "]" && characters[close + 1] == "]") {
                if characters[close] == "[" || characters[close] == "]" { return nil }
                close += 1
            }
            guard close + 1 < characters.count, close > innerStart else { return nil }
            if let bar = characters[innerStart..<close].firstIndex(of: "|") {
                guard bar > innerStart, bar + 1 < close else { return nil }
                return ParsedLink(range: open..<(close + 2), label: (bar + 1)..<close, wikiTarget: innerStart..<bar)
            }
            return ParsedLink(range: open..<(close + 2), label: innerStart..<close, wikiTarget: innerStart..<close)
        }
        var close = open + 1
        while close < characters.count, characters[close] != "]" {
            if characters[close] == "[" { return nil }
            close += 1
        }
        guard close + 1 < characters.count, characters[close + 1] == "(", close > open + 1 else { return nil }
        if asTheDeskKeysIt {
            var depth = 1
            var end = close + 2
            while end < characters.count {
                if characters[end] == "(" {
                    depth += 1
                } else if characters[end] == ")" {
                    depth -= 1
                    if depth == 0 {
                        return ParsedLink(range: open..<(end + 1), label: (open + 1)..<close, destination: (close + 2)..<end)
                    }
                }
                end += 1
            }
            return nil
        }
        var end = close + 2
        while end < characters.count, characters[end] != ")" {
            if characters[end].isWhitespace { return nil }
            end += 1
        }
        guard end < characters.count, end > close + 2 else { return nil }
        return ParsedLink(range: open..<(end + 1), label: (open + 1)..<close, destination: (close + 2)..<end)
    }

    static func inlineExclusions(_ characters: [Character]) -> [Range<Int>] {
        var ranges: [Range<Int>] = []
        var index = 0
        while index < characters.count {
            if characters[index] == "`" {
                let run = characters[index...].prefix { $0 == "`" }.count
                var search = index + run
                var closed: Int?
                while search < characters.count {
                    if characters[search] == "`" {
                        let closing = characters[search...].prefix { $0 == "`" }.count
                        if closing == run {
                            closed = search + closing
                            break
                        }
                        search += closing
                    } else {
                        search += 1
                    }
                }
                if let closed {
                    ranges.append(index..<closed)
                    index = closed
                } else {
                    index += run
                }
                continue
            }
            if characters[index] == "<", matches(characters, at: index, "<!--") {
                var search = index + 4
                var closed: Int?
                while search + 2 < characters.count {
                    if matches(characters, at: search, "-->") {
                        closed = search + 3
                        break
                    }
                    search += 1
                }
                if let closed {
                    ranges.append(index..<closed)
                    index = closed
                    continue
                }
            }
            index += 1
        }
        return ranges
    }

    static func matches(_ characters: [Character], at index: Int, _ token: String) -> Bool {
        let token = Array(token)
        guard index + token.count <= characters.count else { return false }
        for offset in 0..<token.count where characters[index + offset] != token[offset] {
            return false
        }
        return true
    }

    /// The text a line is identified by: list marker, checkbox, time prefix
    /// and trailing comments stripped, whitespace collapsed (V34 API §7.4
    /// steps 1–6). Empty means the line is not addressable.
    public static func lineIdentity(_ line: String) -> String {
        var rest = line.trimmingLeadingWhitespace()
        rest = strippingListMarker(rest)
        for checkbox in ["[ ] ", "[x] ", "[X] "] where rest.hasPrefix(checkbox) {
            rest = rest.dropFirst(checkbox.count)
            break
        }
        if let time = TimePrefix.parse(rest) {
            rest = time.rest
        }
        var text = String(rest)
        while true {
            text = text.trimmingTrailingWhitespace()
            guard text.hasSuffix("-->"), let open = text.range(of: "<!--", options: .backwards) else { break }
            text = String(text[..<open.lowerBound])
        }
        return text.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }

    public static func lineHash(_ line: String) -> String {
        String(sha256Hex(lineIdentity(line)).prefix(16))
    }

    static func strippingListMarker(_ text: Substring) -> Substring {
        for marker in ["- ", "* ", "+ "] where text.hasPrefix(marker) {
            return text.dropFirst(2)
        }
        let digits = text.prefix { $0.isASCII && $0.isNumber }
        if (1...9).contains(digits.count) {
            let after = text.dropFirst(digits.count)
            if after.hasPrefix(". ") || after.hasPrefix(") ") {
                return after.dropFirst(2)
            }
        }
        return text
    }

    /// The hash of a section's body, line endings normalised.
    public static func sectionHash(lines: [String]) -> String {
        String(sha256Hex(lines.joined(separator: "\n")).prefix(16))
    }

    public static func sectionHash(content: String, heading: HeadingRef) -> String? {
        let file = TextFile(content)
        guard let found = file.resolve(heading) else { return nil }
        let section = file.section(of: found)
        return sectionHash(lines: file.lines[section.body].map(\.text))
    }
}

/// The V4 planner time prefix: `H:MM` or `HH:MM`, optionally a separator
/// (`-`, `–`, `—`, `to`) and an end time, followed by whitespace or nothing.
public struct TimePrefix: Equatable, Sendable {
    public var startMinutes: Int
    public var endMinutes: Int?
    /// The prefix exactly as written, without the whitespace after it.
    public var raw: String
    public var rest: Substring

    public static func parse(_ text: Substring) -> TimePrefix? {
        guard let (start, afterStart) = parseTime(text, allowMidnightEnd: false) else { return nil }
        if let afterSeparator = strippingSeparator(afterStart),
           let (end, afterEnd) = parseTime(afterSeparator, allowMidnightEnd: true),
           let rest = label(after: afterEnd)
        {
            let raw = String(text[text.startIndex..<afterEnd.startIndex])
            return TimePrefix(startMinutes: start, endMinutes: end, raw: raw, rest: rest)
        }
        guard let rest = label(after: afterStart) else { return nil }
        let raw = String(text[text.startIndex..<afterStart.startIndex])
        return TimePrefix(startMinutes: start, endMinutes: nil, raw: raw, rest: rest)
    }

    public static func parse(_ text: String) -> TimePrefix? {
        parse(Substring(text))
    }

    private static func parseTime(_ text: Substring, allowMidnightEnd: Bool) -> (Int, Substring)? {
        if allowMidnightEnd, text.hasPrefix("24:00") {
            return (24 * 60, text.dropFirst(5))
        }
        let hourDigits = text.prefix(2).prefix { $0.isASCII && $0.isNumber }
        guard !hourDigits.isEmpty else { return nil }
        var rest = text.dropFirst(hourDigits.count)
        guard rest.first == ":" else { return nil }
        rest = rest.dropFirst()
        let minuteDigits = rest.prefix(2)
        guard minuteDigits.count == 2, minuteDigits.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
        guard let hours = Int(hourDigits), let minutes = Int(minuteDigits), hours <= 23, minutes <= 59 else { return nil }
        return (hours * 60 + minutes, rest.dropFirst(2))
    }

    private static func strippingSeparator(_ text: Substring) -> Substring? {
        let trimmed = text.trimmingLeadingWhitespace()
        for separator in ["–", "—", "-", "to"] where trimmed.hasPrefix(separator) {
            return trimmed.dropFirst(separator.count).trimmingLeadingWhitespace()
        }
        return nil
    }

    /// A time must be followed by whitespace or the end of the line, otherwise
    /// it was not a time (`9:30am`).
    private static func label(after token: Substring) -> Substring? {
        if token.isEmpty { return token }
        let rest = token.trimmingLeadingWhitespace()
        return rest.count < token.count ? rest : nil
    }

    /// `09:30` or `09:30 - 11:00`, the form the phone writes.
    public static func format(startMinutes: Int, endMinutes: Int?) -> String {
        func clock(_ minutes: Int) -> String {
            String(format: "%02d:%02d", minutes / 60, minutes % 60)
        }
        if let endMinutes {
            return "\(clock(startMinutes)) - \(clock(endMinutes))"
        }
        return clock(startMinutes)
    }
}
