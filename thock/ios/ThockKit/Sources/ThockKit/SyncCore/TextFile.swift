import Foundation

/// One line of a note with the terminator it was read with, so a file can be
/// edited line by line and written back byte for byte (V34 API §8.1).
public struct TextLine: Equatable, Sendable {
    public var text: String
    /// `"\n"`, `"\r\n"`, or `""` for a last line without one.
    public var terminator: String

    public init(text: String, terminator: String) {
        self.text = text
        self.terminator = terminator
    }

    public var isBlank: Bool {
        text.allSatisfy(\.isWhitespace)
    }
}

/// An ATX heading located in a file.
public struct HeadingLine: Equatable, Sendable {
    public var index: Int
    public var level: Int
    public var text: String
}

/// The line range of a section: everything after its heading up to the next
/// heading of the same or a higher level (V34 API §7.2).
public struct SectionRange: Equatable, Sendable {
    /// The heading's own line, `nil` when the section is the whole file.
    public var headingIndex: Int?
    public var level: Int
    public var start: Int
    public var end: Int
    /// `start..<bodyEnd` is the section without its trailing blank lines.
    public var bodyEnd: Int
    /// `start..<ownEnd` is the body before the first deeper heading, without
    /// trailing blank lines.
    public var ownEnd: Int

    public var body: Range<Int> { start..<bodyEnd }
    public var own: Range<Int> { start..<ownEnd }
}

public struct TextFile: Equatable, Sendable {
    public var lines: [TextLine]
    /// The terminator inserted lines use: the one the file had most of when
    /// it was read, `"\n"` for a new file.
    public let lineEnding: String

    public init(_ text: String) {
        var lines: [TextLine] = []
        var current = ""
        var crlf = 0
        var lf = 0
        for character in text {
            if character == "\r\n" {
                lines.append(TextLine(text: current, terminator: "\r\n"))
                current = ""
                crlf += 1
            } else if character == "\n" {
                lines.append(TextLine(text: current, terminator: "\n"))
                current = ""
                lf += 1
            } else {
                current.append(character)
            }
        }
        if !current.isEmpty {
            lines.append(TextLine(text: current, terminator: ""))
        }
        self.lines = lines
        self.lineEnding = crlf > lf ? "\r\n" : "\n"
    }

    public var text: String {
        var output = ""
        for line in lines {
            output += line.text
            output += line.terminator
        }
        return output
    }

    /// Inserts `newLines` before `index`. A file whose last line has no
    /// terminator keeps that property for whichever line ends up last.
    public mutating func insert(_ newLines: [String], at index: Int) {
        guard !newLines.isEmpty else { return }
        let index = min(max(index, 0), lines.count)
        var inserted = newLines.map { TextLine(text: $0, terminator: lineEnding) }
        if index == lines.count, let last = lines.last, last.terminator.isEmpty {
            lines[lines.count - 1].terminator = lineEnding
            inserted[inserted.count - 1].terminator = ""
        }
        lines.insert(contentsOf: inserted, at: index)
    }

    /// Replaces a run of lines. The new lines go in as an insertion would,
    /// so only a last line that is still there keeps its missing terminator.
    public mutating func replace(_ range: Range<Int>, with newLines: [String]) {
        lines.removeSubrange(range)
        insert(newLines, at: range.lowerBound)
    }

    /// Lines inside frontmatter or a fenced code block are never headings and
    /// never the target of a write.
    public func contentMask() -> [Bool] {
        var mask = [Bool](repeating: true, count: lines.count)
        var inFrontmatter = false
        // A front-matter block that never closes is ordinary content.
        let closesFrontmatter = lines.dropFirst().contains {
            let trimmed = $0.text.trimmingTrailingWhitespace()
            return trimmed == "---" || trimmed == "..."
        }
        var fence: (marker: Character, length: Int)?
        for (index, line) in lines.enumerated() {
            let trimmedEnd = line.text.trimmingTrailingWhitespace()
            if index == 0, trimmedEnd == "---", closesFrontmatter {
                inFrontmatter = true
                mask[index] = false
                continue
            }
            if inFrontmatter {
                mask[index] = false
                if trimmedEnd == "---" || trimmedEnd == "..." {
                    inFrontmatter = false
                }
                continue
            }
            if let open = fence {
                mask[index] = false
                if Self.closesFence(line.text, marker: open.marker, length: open.length) {
                    fence = nil
                }
                continue
            }
            if let opened = Self.opensFence(line.text) {
                fence = opened
                mask[index] = false
            }
        }
        return mask
    }

    static func opensFence(_ line: String) -> (marker: Character, length: Int)? {
        let indent = line.prefix { $0 == " " }.count
        guard indent <= 3 else { return nil }
        let rest = line.dropFirst(indent)
        guard let marker = rest.first, marker == "`" || marker == "~" else { return nil }
        let length = rest.prefix { $0 == marker }.count
        guard length >= 3 else { return nil }
        if marker == "`", rest.dropFirst(length).contains("`") { return nil }
        return (marker, length)
    }

    static func closesFence(_ line: String, marker: Character, length: Int) -> Bool {
        let indent = line.prefix { $0 == " " }.count
        guard indent <= 3 else { return false }
        let rest = line.dropFirst(indent)
        let run = rest.prefix { $0 == marker }.count
        guard run >= length else { return false }
        return rest.dropFirst(run).allSatisfy(\.isWhitespace)
    }

    /// `___`, `---` or `***`: three or more of one character, spaces allowed.
    public static func isThematicBreak(_ line: String) -> Bool {
        let compact = line.filter { !$0.isWhitespace }
        guard compact.count >= 3, let first = compact.first, "_-*".contains(first) else { return false }
        return compact.allSatisfy { $0 == first }
    }

    /// `^#{1,6}[ \t]+\S`, the heading text trimmed and without closing hashes.
    public static func heading(in line: String) -> (level: Int, text: String)? {
        let level = line.prefix { $0 == "#" }.count
        guard (1...6).contains(level) else { return nil }
        let rest = line.dropFirst(level)
        guard let first = rest.first, first == " " || first == "\t" else { return nil }
        var text = rest.trimmingCharacters(in: .whitespaces)
        guard !text.isEmpty else { return nil }
        // Closing hashes (`## Day planner ##`) are decoration, not text.
        let closing = text.reversed().prefix { $0 == "#" }.count
        if closing > 0, closing < text.count {
            let before = text.dropLast(closing)
            if before.last == " " || before.last == "\t" {
                text = before.trimmingCharacters(in: .whitespaces)
            }
        }
        return (level, text)
    }

    public func headings() -> [HeadingLine] {
        let mask = contentMask()
        var found: [HeadingLine] = []
        for (index, line) in lines.enumerated() where mask[index] {
            if let heading = Self.heading(in: line.text) {
                found.append(HeadingLine(index: index, level: heading.level, text: heading.text))
            }
        }
        return found
    }

    /// Resolves a heading by exact lowercase text first, then by its
    /// normalised key, ignoring level; `ordinal` picks among the matches and
    /// falls back to the last when out of range (V34 API §7.2).
    public func resolve(_ reference: HeadingRef) -> HeadingLine? {
        resolve(names: [reference.text], ordinal: reference.ordinal)
    }

    public func resolve(names: [String], ordinal: Int = 0) -> HeadingLine? {
        let all = headings()
        for name in names {
            let wanted = name.trimmingCharacters(in: .whitespaces).lowercased()
            let exact = all.filter { $0.text.lowercased() == wanted }
            if !exact.isEmpty {
                return exact[min(max(ordinal, 0), exact.count - 1)]
            }
        }
        for name in names {
            let key = SyncCore.headingKey(name)
            guard !key.isEmpty else { continue }
            let normalised = all.filter { SyncCore.headingKey($0.text) == key }
            if !normalised.isEmpty {
                return normalised[min(max(ordinal, 0), normalised.count - 1)]
            }
        }
        return nil
    }

    public func section(of heading: HeadingLine) -> SectionRange {
        let all = headings()
        let start = heading.index + 1
        let end = all.first { $0.index > heading.index && $0.level <= heading.level }?.index ?? lines.count
        return range(headingIndex: heading.index, level: heading.level, start: start, end: end, headings: all)
    }

    /// The whole file as one section, for writes whose heading is `null`.
    public func wholeFile() -> SectionRange {
        range(headingIndex: nil, level: 0, start: 0, end: lines.count, headings: [])
    }

    private func range(headingIndex: Int?, level: Int, start: Int, end: Int, headings: [HeadingLine]) -> SectionRange {
        var bodyEnd = end
        while bodyEnd > start, lines[bodyEnd - 1].isBlank {
            bodyEnd -= 1
        }
        // The templates close each section with a rule. It belongs to the
        // page's layout, not to the section's content, so appends land above it.
        if headingIndex != nil, bodyEnd > start, Self.isThematicBreak(lines[bodyEnd - 1].text) {
            bodyEnd -= 1
            while bodyEnd > start, lines[bodyEnd - 1].isBlank {
                bodyEnd -= 1
            }
        }
        var ownEnd = headings.first { $0.index >= start && $0.index < bodyEnd }?.index ?? bodyEnd
        while ownEnd > start, lines[ownEnd - 1].isBlank {
            ownEnd -= 1
        }
        if headingIndex != nil, ownEnd > start, Self.isThematicBreak(lines[ownEnd - 1].text) {
            ownEnd -= 1
            while ownEnd > start, lines[ownEnd - 1].isBlank {
                ownEnd -= 1
            }
        }
        return SectionRange(headingIndex: headingIndex, level: level, start: start, end: end, bodyEnd: bodyEnd, ownEnd: ownEnd)
    }
}

extension StringProtocol {
    func trimmingTrailingWhitespace() -> String {
        var view = Substring(self)
        while let last = view.last, last.isWhitespace {
            view.removeLast()
        }
        return String(view)
    }

    func trimmingLeadingWhitespace() -> Substring {
        var view = Substring(self)
        while let first = view.first, first.isWhitespace {
            view.removeFirst()
        }
        return view
    }
}
