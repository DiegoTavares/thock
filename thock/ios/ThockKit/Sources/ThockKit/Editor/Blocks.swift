import Foundation

public enum BlockKind: Equatable, Hashable, Sendable {
    case paragraph
    case heading(level: Int)
    case bullet
    case task(checked: Bool)
    case numbered(marker: String)
    case quote
    case rule
    case blank
    /// A table, code block, comment, frontmatter or raw HTML: shown as a grey
    /// block, preserved exactly, never editable on the phone.
    case opaque

    public var isEditable: Bool {
        switch self {
        case .paragraph, .heading, .bullet, .task, .quote: return true
        default: return false
        }
    }
}

/// One block of a note, with the source lines it was read from. A block the
/// user never touches is written back from `source`, byte for byte.
public struct Block: Equatable, Hashable, Identifiable, Sendable {
    public var id: Int
    public var kind: BlockKind
    /// The inline Markdown without the block's own prefix; soft-wrapped
    /// paragraph lines are joined by `\n`.
    public var text: String
    /// Leading whitespace of a list item, kept so nested items stay nested.
    public var indent: String
    public var source: [String]
    /// The first line's index in the text the block was parsed from.
    public var line: Int
    public var touched: Bool

    public init(id: Int, kind: BlockKind, text: String, indent: String = "", source: [String] = [], line: Int = 0, touched: Bool = false) {
        self.id = id
        self.kind = kind
        self.text = text
        self.indent = indent
        self.source = source
        self.line = line
        self.touched = touched
    }

    public var runs: [InlineRun] {
        Inline.parse(text.replacingOccurrences(of: "\n", with: " "))
    }

    /// A list item's time prefix, drawn as a chip.
    public var time: TimePrefix? {
        guard case .task = kind else { return nil }
        return TimePrefix.parse(text)
    }

    /// The Markdown this block is written as when it was touched or is new.
    public func markdownLines() -> [String] {
        switch kind {
        case .paragraph: return text.components(separatedBy: "\n")
        case .heading(let level): return [String(repeating: "#", count: level) + " " + text]
        case .bullet: return [indent + "- " + text]
        case .task(let checked): return [indent + (checked ? "- [x] " : "- [ ] ") + text]
        case .numbered(let marker): return [indent + marker + " " + text]
        case .quote: return text.components(separatedBy: "\n").map { $0.isEmpty ? ">" : "> " + $0 }
        case .rule: return ["___"]
        case .blank: return [""]
        case .opaque: return source
        }
    }
}

public enum Blocks {
    /// Splits Markdown into blocks. `firstLine` offsets the recorded line
    /// numbers when `lines` is a slice of a larger file.
    public static func parse(_ text: String) -> [Block] {
        parse(lines: TextFile(text).lines.map(\.text), firstLine: 0)
    }

    public static func parse(lines: [String], firstLine: Int = 0, alreadyInsideNote: Bool = false) -> [Block] {
        var blocks: [Block] = []
        var index = 0

        func add(_ kind: BlockKind, text: String, indent: String = "", from start: Int, to end: Int) {
            blocks.append(Block(id: blocks.count, kind: kind, text: text, indent: indent, source: Array(lines[start..<end]), line: firstLine + start))
        }

        let closesFrontmatter = lines.dropFirst().contains {
            let trimmed = $0.trimmingTrailingWhitespace()
            return trimmed == "---" || trimmed == "..."
        }
        if !alreadyInsideNote, firstLine == 0, lines.first?.trimmingTrailingWhitespace() == "---", closesFrontmatter {
            var end = 1
            while end < lines.count {
                let trimmed = lines[end].trimmingTrailingWhitespace()
                end += 1
                if trimmed == "---" || trimmed == "..." { break }
            }
            add(.opaque, text: "", from: 0, to: end)
            index = end
        }

        while index < lines.count {
            let line = lines[index]
            let trimmed = line.trimmingCharacters(in: .whitespaces)

            if trimmed.isEmpty {
                add(.blank, text: "", from: index, to: index + 1)
                index += 1
                continue
            }

            if let fence = TextFile.opensFence(line) {
                var end = index + 1
                while end < lines.count {
                    let closes = TextFile.closesFence(lines[end], marker: fence.marker, length: fence.length)
                    end += 1
                    if closes { break }
                }
                add(.opaque, text: "", from: index, to: end)
                index = end
                continue
            }

            if trimmed.hasPrefix("<!--") {
                var end = index
                while end < lines.count, !lines[end].contains("-->") {
                    end += 1
                }
                end = min(end + 1, lines.count)
                add(.opaque, text: "", from: index, to: end)
                index = end
                continue
            }

            if let heading = TextFile.heading(in: line) {
                add(.heading(level: heading.level), text: heading.text, from: index, to: index + 1)
                index += 1
                continue
            }

            if isRule(trimmed) {
                add(.rule, text: "", from: index, to: index + 1)
                index += 1
                continue
            }

            if trimmed.hasPrefix("|") || isRawHTML(trimmed) {
                var end = index + 1
                while end < lines.count, !lines[end].trimmingCharacters(in: .whitespaces).isEmpty,
                      trimmed.hasPrefix("|") ? lines[end].trimmingCharacters(in: .whitespaces).hasPrefix("|") : true
                {
                    end += 1
                }
                add(.opaque, text: "", from: index, to: end)
                index = end
                continue
            }

            if trimmed.hasPrefix(">") {
                var end = index
                var quoted: [String] = []
                while end < lines.count, lines[end].trimmingCharacters(in: .whitespaces).hasPrefix(">") {
                    var content = lines[end].trimmingCharacters(in: .whitespaces).dropFirst()
                    if content.hasPrefix(" ") { content = content.dropFirst() }
                    quoted.append(String(content))
                    end += 1
                }
                add(.quote, text: quoted.joined(separator: "\n"), from: index, to: end)
                index = end
                continue
            }

            if let item = listItem(line) {
                add(item.kind, text: item.text, indent: item.indent, from: index, to: index + 1)
                index += 1
                continue
            }

            var end = index + 1
            while end < lines.count, continuesParagraph(lines[end]) {
                end += 1
            }
            let text = lines[index..<end].map { $0.trimmingCharacters(in: .whitespaces) }.joined(separator: "\n")
            add(.paragraph, text: text, from: index, to: end)
            index = end
        }
        return blocks
    }

    public static func isRule(_ trimmed: String) -> Bool {
        TextFile.isThematicBreak(trimmed)
    }

    static func isRawHTML(_ trimmed: String) -> Bool {
        guard trimmed.hasPrefix("<"), trimmed.count > 1 else { return false }
        let second = trimmed[trimmed.index(after: trimmed.startIndex)]
        return second.isLetter || second == "/"
    }

    struct ListItem {
        var kind: BlockKind
        var text: String
        var indent: String
    }

    static func listItem(_ line: String) -> ListItem? {
        let indent = String(line.prefix { $0 == " " || $0 == "\t" })
        let rest = line.dropFirst(indent.count)
        if let marker = rest.first, "-*+".contains(marker) {
            let afterMarker = rest.dropFirst()
            guard afterMarker.first == " " || afterMarker.first == "\t" else { return nil }
            let content = afterMarker.trimmingLeadingWhitespace()
            for (box, checked) in [("[ ]", false), ("[x]", true), ("[X]", true)] where content.hasPrefix(box) {
                let afterBox = content.dropFirst(3)
                if afterBox.isEmpty || afterBox.first == " " || afterBox.first == "\t" {
                    return ListItem(kind: .task(checked: checked), text: String(afterBox.trimmingLeadingWhitespace()), indent: indent)
                }
            }
            return ListItem(kind: .bullet, text: String(content), indent: indent)
        }
        let digits = rest.prefix { $0.isASCII && $0.isNumber }
        if (1...9).contains(digits.count) {
            let after = rest.dropFirst(digits.count)
            if let punctuation = after.first, ".)".contains(punctuation), after.dropFirst().first == " " {
                return ListItem(kind: .numbered(marker: String(digits) + String(punctuation)), text: String(after.dropFirst(2).trimmingLeadingWhitespace()), indent: indent)
            }
        }
        return nil
    }

    private static func continuesParagraph(_ line: String) -> Bool {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty || trimmed.hasPrefix(">") || trimmed.hasPrefix("|") || trimmed.hasPrefix("<!--") { return false }
        if TextFile.heading(in: line) != nil || isRule(trimmed) || listItem(line) != nil || TextFile.opensFence(line) != nil { return false }
        return true
    }
}

/// A note, or a slice of one, open in the editor. Saving emits the blocks the
/// user touched from the subset and copies every other block through
/// unchanged (V33 §8, round-tripping).
public struct EditorDocument: Equatable, Sendable {
    public var blocks: [Block]

    public init(markdown: String) {
        blocks = Blocks.parse(markdown)
    }

    public init(lines: [String]) {
        blocks = Blocks.parse(lines: lines, alreadyInsideNote: true)
    }

    public init(blocks: [Block]) {
        self.blocks = blocks
    }

    public func lines() -> [String] {
        var output: [String] = []
        for (index, block) in blocks.enumerated() {
            if block.touched || block.source.isEmpty {
                if needsBlankLine(before: index, output: output) {
                    output.append("")
                }
                output += block.markdownLines()
            } else {
                // A block kept as it was still needs the blank line that
                // separates it from a new one written just before it.
                if index > 0, blocks[index - 1].source.isEmpty, block.kind != .blank, needsBlankLine(before: index, output: output) {
                    output.append("")
                }
                output += block.source
            }
        }
        return output
    }

    /// New and edited paragraphs get the one blank line Markdown needs to
    /// keep them apart; list items sit together.
    private func needsBlankLine(before index: Int, output: [String]) -> Bool {
        guard index > 0, let last = output.last, !last.trimmingCharacters(in: .whitespaces).isEmpty else { return false }
        let block = blocks[index]
        let previous = blocks[index - 1]
        guard previous.touched || previous.source.isEmpty || block.source.isEmpty else { return false }
        switch (previous.kind, block.kind) {
        case (.bullet, .bullet), (.task, .task), (.bullet, .task), (.task, .bullet), (.numbered, .numbered):
            return false
        default:
            return true
        }
    }

    public func markdown(lineEnding: String = "\n") -> String {
        lines().map { $0 + lineEnding }.joined()
    }
}
