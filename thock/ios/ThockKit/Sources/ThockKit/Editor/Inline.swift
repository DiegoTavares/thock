import Foundation

public enum LinkTarget: Equatable, Hashable, Sendable {
    /// `[[target]]` or `[[target|label]]`.
    case note(String)
    /// `[label](destination)`.
    case web(String)
}

/// A run of inline text with the handful of styles the desk conceals (V10,
/// V24). This is the editor's whole inline vocabulary.
public struct InlineRun: Equatable, Hashable, Sendable {
    public var text: String
    public var bold = false
    public var italic = false
    public var code = false
    public var strike = false
    public var link: LinkTarget?
    /// The run is an HTML comment (`<!--gcal:…-->`), its text the verbatim
    /// source including the spaces before it. It is never shown, and is
    /// written back as it was so the ids it carries survive an edit.
    public var comment = false

    public init(_ text: String, bold: Bool = false, italic: Bool = false, code: Bool = false, strike: Bool = false, link: LinkTarget? = nil, comment: Bool = false) {
        self.text = text
        self.bold = bold
        self.italic = italic
        self.code = code
        self.strike = strike
        self.link = link
        self.comment = comment
    }
}

public enum Inline {
    /// Parses one block's inline Markdown. Constructs outside the subset
    /// (images, raw HTML) stay literal text, so they survive an edit as typed.
    /// Single-line HTML comments become `comment` runs, hidden from the display.
    public static func parse(_ text: String) -> [InlineRun] {
        var runs: [InlineRun] = []
        parse(Array(text), style: InlineRun(""), into: &runs)
        return merged(runs)
    }

    public static func plainText(_ text: String) -> String {
        parse(text).filter { !$0.comment }.map(\.text).joined()
    }

    private static func parse(_ characters: [Character], style: InlineRun, into runs: inout [InlineRun]) {
        var literal = ""
        var index = 0

        func flush(_ runs: inout [InlineRun]) {
            guard !literal.isEmpty else { return }
            var run = style
            run.text = literal
            runs.append(run)
            literal = ""
        }

        while index < characters.count {
            if isEscape(characters, at: index) {
                literal.append(characters[index + 1])
                index += 2
                continue
            }

            if let span = codeSpan(characters, at: index) {
                flush(&runs)
                var content = characters[(index + span.fence)..<span.close]
                // The padding that lets a code span start or end with a backtick.
                if content.count > 1, content.first == " ", content.last == " ", content.contains(where: { $0 != " " }) {
                    content = content.dropFirst().dropLast()
                }
                var run = style
                run.code = true
                run.text = String(content)
                runs.append(run)
                index = span.close + span.fence
                continue
            }

            if let end = commentEnd(characters, at: index) {
                // The space before a trailing marker goes with it.
                var spaces = ""
                while literal.hasSuffix(" ") {
                    literal.removeLast()
                    spaces.append(" ")
                }
                flush(&runs)
                var run = style
                run.comment = true
                run.text = spaces + String(characters[index..<end])
                runs.append(run)
                index = end
                continue
            }

            if let parsed = link(characters, at: index) {
                flush(&runs)
                var linked = style
                if let target = parsed.wikiTarget {
                    linked.link = .note(String(characters[target]))
                } else if let destination = parsed.destination {
                    linked.link = .web(String(characters[destination]))
                }
                parse(Array(characters[parsed.label]), style: linked, into: &runs)
                index = parsed.range.upperBound
                continue
            }

            if let span = emphasis(characters, at: index) {
                flush(&runs)
                var styled = style
                span.apply(&styled)
                parse(Array(characters[(index + span.token.count)..<span.close]), style: styled, into: &runs)
                index = span.close + span.token.count
                continue
            }

            literal.append(characters[index])
            index += 1
        }
        flush(&runs)
    }

    /// A backslash escapes a markup character, and a `<` that would open a
    /// comment.
    private static func isEscape(_ characters: [Character], at index: Int) -> Bool {
        guard characters[index] == "\\", index + 1 < characters.count else { return false }
        return "*_`[]~\\".contains(characters[index + 1]) || SyncCore.matches(characters, at: index + 1, "<!--")
    }

    private static func codeSpan(_ characters: [Character], at index: Int) -> (fence: Int, close: Int)? {
        guard characters[index] == "`" else { return nil }
        let fence = characters[index...].prefix { $0 == "`" }.count
        guard let close = closing(characters, from: index + fence, token: String(repeating: "`", count: fence)) else { return nil }
        return (fence, close)
    }

    /// The index just past a `<!-- … -->` that starts at `index`.
    private static func commentEnd(_ characters: [Character], at index: Int) -> Int? {
        guard SyncCore.matches(characters, at: index, "<!--"),
              let close = closing(characters, from: index + 4, token: "-->")
        else { return nil }
        return close + 3
    }

    private static func link(_ characters: [Character], at index: Int) -> SyncCore.ParsedLink? {
        guard characters[index] == "[", !(index > 0 && characters[index - 1] == "!") else { return nil }
        return SyncCore.parseLink(characters, at: index)
    }

    private static func emphasis(_ characters: [Character], at index: Int) -> (token: String, apply: (inout InlineRun) -> Void, close: Int)? {
        guard let (token, apply) = delimiter(characters, at: index),
              let close = closing(characters, from: index + token.count, token: token),
              close > index + token.count,
              !characters[index + token.count].isWhitespace,
              !characters[close - 1].isWhitespace,
              wordBoundaryAllows(token, characters, open: index, close: close)
        else { return nil }
        return (token, apply, close)
    }

    private static func delimiter(_ characters: [Character], at index: Int) -> (String, (inout InlineRun) -> Void)? {
        if SyncCore.matches(characters, at: index, "**") { return ("**", { $0.bold = true }) }
        if SyncCore.matches(characters, at: index, "__") { return ("__", { $0.bold = true }) }
        if SyncCore.matches(characters, at: index, "~~") { return ("~~", { $0.strike = true }) }
        if characters[index] == "*" { return ("*", { $0.italic = true }) }
        if characters[index] == "_" { return ("_", { $0.italic = true }) }
        return nil
    }

    /// An underscore inside a word (`snake_case`) is not emphasis.
    private static func wordBoundaryAllows(_ token: String, _ characters: [Character], open: Int, close: Int) -> Bool {
        guard token.first == "_" else { return true }
        let before = open > 0 ? characters[open - 1] : " "
        let afterIndex = close + token.count
        let after = afterIndex < characters.count ? characters[afterIndex] : " "
        return !(before.isLetter || before.isNumber) && !(after.isLetter || after.isNumber)
    }

    private static func closing(_ characters: [Character], from start: Int, token: String) -> Int? {
        var index = start
        while index < characters.count {
            if characters[index] == "\\" {
                index += 2
                continue
            }
            if SyncCore.matches(characters, at: index, token) {
                // A lone `*` must not close on half of a `**`.
                if token == "*", index + 1 < characters.count, characters[index + 1] == "*" {
                    index += 2
                    continue
                }
                return index
            }
            index += 1
        }
        return nil
    }

    static func merged(_ runs: [InlineRun]) -> [InlineRun] {
        var output: [InlineRun] = []
        for run in runs where !run.text.isEmpty {
            if var last = output.last, last.bold == run.bold, last.italic == run.italic, last.code == run.code, last.strike == run.strike, last.link == run.link, last.comment == run.comment {
                last.text += run.text
                output[output.count - 1] = last
            } else {
                output.append(run)
            }
        }
        return output
    }

    /// Writes runs back as the one Markdown form each style maps to (V33 §8).
    /// Literal markup characters are escaped, so the text reads back as the
    /// same runs.
    public static func serialize(_ runs: [InlineRun]) -> String {
        let parts = segments(merged(runs))[...]
        let needed = serialize(parts, level: 0, escaping: .needed)
        let all = serialize(parts, level: 0, escaping: .all)
        // Escaping only what would read as markup on its own keeps the person's
        // text as typed, but the markers around a run can change that reading.
        return parse(needed) == parse(all) ? needed : all
    }

    private enum Escaping {
        /// Only characters that would read as markup within their own run.
        case needed
        /// Every character that can take part in markup.
        case all
    }

    /// A link with all the runs of its label, or a single run outside a link.
    private struct Segment {
        var link: LinkTarget?
        var runs: [InlineRun]
    }

    private static func segments(_ runs: [InlineRun]) -> [Segment] {
        var output: [Segment] = []
        for run in runs {
            if let link = run.link, output.last?.link == link {
                output[output.count - 1].runs.append(run)
            } else {
                output.append(Segment(link: run.link, runs: [run]))
            }
        }
        return output
    }

    private static let wrappers: [(marker: String, style: WritableKeyPath<InlineRun, Bool>)] = [
        ("**", \.bold),
        ("_", \.italic),
        ("~~", \.strike),
    ]

    /// Neighbouring runs that share a style share its markers, so bold text
    /// with one italic word stays one bold span. A style goes outside a link
    /// only when its whole label has it, so a link is never split.
    private static func serialize(_ segments: ArraySlice<Segment>, level: Int, escaping: Escaping) -> String {
        guard level < wrappers.count else {
            return segments.map { written($0, escaping: escaping) }.joined()
        }
        let wrapper = wrappers[level]
        func isOn(_ segment: Segment) -> Bool {
            segment.runs.allSatisfy { $0[keyPath: wrapper.style] }
        }
        var output = ""
        var index = segments.startIndex
        while index < segments.endIndex {
            let on = isOn(segments[index])
            var end = index
            while end < segments.endIndex, isOn(segments[end]) == on {
                end += 1
            }
            let inner = serialize(segments[index..<end], level: level + 1, escaping: escaping)
            output += on ? hugging(inner, { wrapper.marker + String($0) + wrapper.marker }) : inner
            index = end
        }
        return output
    }

    private static func written(_ segment: Segment, escaping: Escaping) -> String {
        guard let link = segment.link else {
            return segment.runs.map { leaf($0, escaping: escaping) }.joined()
        }
        var label = segment.runs
        for index in label.indices {
            label[index].link = nil
        }
        for wrapper in wrappers where label.allSatisfy({ $0[keyPath: wrapper.style] }) {
            for index in label.indices {
                label[index][keyPath: wrapper.style] = false
            }
        }
        let text = serialize(segments(label)[...], level: 0, escaping: escaping)
        switch link {
        case .note(let target):
            return parse(target) == merged(label) ? "[[\(target)]]" : "[[\(target)|\(text)]]"
        case .web(let destination):
            return "[\(text)](\(destination))"
        }
    }

    /// Markers hug the words: `**bold** ` rather than `**bold **`, which the
    /// desk would not read as bold.
    private static func hugging(_ text: String, _ wrap: (Substring) -> String) -> String {
        let leading = String(text.prefix { $0 == " " })
        let trailing = String(text.reversed().prefix { $0 == " " })
        let core = text.dropFirst(leading.count).dropLast(trailing.count)
        guard !core.isEmpty else { return text }
        return leading + wrap(core) + trailing
    }

    private static func leaf(_ run: InlineRun, escaping: Escaping) -> String {
        if run.comment {
            return run.text
        }
        if run.code {
            return hugging(run.text, fenced)
        }
        return escaped(run.text, escaping)
    }

    /// The fence is longer than any backtick run inside, since parse() closes
    /// a code span on the first match of its fence. Padding keeps a backtick
    /// at either end, or a closing backslash, off the fence.
    private static func fenced(_ code: Substring) -> String {
        var longest = 0
        var current = 0
        for character in code {
            current = character == "`" ? current + 1 : 0
            longest = max(longest, current)
        }
        let fence = String(repeating: "`", count: longest + 1)
        let padding = code.first == "`" || code.last == "`" || code.last == "\\" ? " " : ""
        return fence + padding + String(code) + padding + fence
    }

    private static func escaped(_ text: String, _ escaping: Escaping) -> String {
        let characters = Array(text)
        var output = ""
        for index in characters.indices {
            let escape: Bool
            switch escaping {
            case .needed:
                escape = opensMarkup(characters, at: index)
            case .all:
                escape = "*_`[]~\\".contains(characters[index]) || SyncCore.matches(characters, at: index, "<!--")
            }
            if escape {
                output.append("\\")
            }
            output.append(characters[index])
        }
        return output
    }

    /// Whether parse() would read the character at `index` as the start of
    /// markup rather than as text.
    private static func opensMarkup(_ characters: [Character], at index: Int) -> Bool {
        isEscape(characters, at: index)
            || codeSpan(characters, at: index) != nil
            || commentEnd(characters, at: index) != nil
            || link(characters, at: index) != nil
            || emphasis(characters, at: index) != nil
    }
}
