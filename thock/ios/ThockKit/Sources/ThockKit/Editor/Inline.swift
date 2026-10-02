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

    public init(_ text: String, bold: Bool = false, italic: Bool = false, code: Bool = false, strike: Bool = false, link: LinkTarget? = nil) {
        self.text = text
        self.bold = bold
        self.italic = italic
        self.code = code
        self.strike = strike
        self.link = link
    }
}

public enum Inline {
    /// Parses one block's inline Markdown. Constructs outside the subset
    /// (images, raw HTML) stay literal text, so they survive an edit as typed.
    /// Single-line HTML comments are dropped from the display.
    public static func parse(_ text: String) -> [InlineRun] {
        var runs: [InlineRun] = []
        parse(Array(text), style: InlineRun(""), into: &runs)
        return merged(runs)
    }

    public static func plainText(_ text: String) -> String {
        parse(text).map(\.text).joined()
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
            let character = characters[index]

            if character == "\\", index + 1 < characters.count, "*_`[]~\\".contains(characters[index + 1]) {
                literal.append(characters[index + 1])
                index += 2
                continue
            }

            if character == "`" {
                let length = characters[index...].prefix { $0 == "`" }.count
                if let close = closing(characters, from: index + length, token: String(repeating: "`", count: length)) {
                    flush(&runs)
                    var run = style
                    run.code = true
                    run.text = String(characters[(index + length)..<close])
                    runs.append(run)
                    index = close + length
                    continue
                }
            }

            if character == "<", SyncCore.matches(characters, at: index, "<!--"),
               let close = closing(characters, from: index + 4, token: "-->")
            {
                // The space before a trailing marker goes with it.
                while literal.hasSuffix(" ") { literal.removeLast() }
                index = close + 3
                continue
            }

            if character == "[", !(index > 0 && characters[index - 1] == "!"),
               let link = SyncCore.parseLink(characters, at: index)
            {
                flush(&runs)
                var linked = style
                if let target = link.wikiTarget {
                    linked.link = .note(String(characters[target]))
                } else if let destination = link.destination {
                    linked.link = .web(String(characters[destination]))
                }
                parse(Array(characters[link.label]), style: linked, into: &runs)
                index = link.range.upperBound
                continue
            }

            if let (token, apply) = delimiter(characters, at: index),
               let close = closing(characters, from: index + token.count, token: token),
               close > index + token.count,
               !characters[index + token.count].isWhitespace,
               !characters[close - 1].isWhitespace,
               wordBoundaryAllows(token, characters, open: index, close: close)
            {
                flush(&runs)
                var styled = style
                apply(&styled)
                parse(Array(characters[(index + token.count)..<close]), style: styled, into: &runs)
                index = close + token.count
                continue
            }

            literal.append(character)
            index += 1
        }
        flush(&runs)
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
            if var last = output.last, last.bold == run.bold, last.italic == run.italic, last.code == run.code, last.strike == run.strike, last.link == run.link {
                last.text += run.text
                output[output.count - 1] = last
            } else {
                output.append(run)
            }
        }
        return output
    }

    /// Writes runs back as the one Markdown form each style maps to (V33 §8).
    public static func serialize(_ runs: [InlineRun]) -> String {
        serialize(merged(runs)[...], level: 0)
    }

    private static let wrappers: [(marker: String, isOn: (InlineRun) -> Bool)] = [
        ("**", { $0.bold }),
        ("_", { $0.italic }),
        ("~~", { $0.strike }),
    ]

    /// Neighbouring runs that share a style share its markers, so bold text
    /// with one italic word stays one bold span.
    private static func serialize(_ runs: ArraySlice<InlineRun>, level: Int) -> String {
        guard level < wrappers.count else {
            return runs.map(leaf).joined()
        }
        let wrapper = wrappers[level]
        var output = ""
        var index = runs.startIndex
        while index < runs.endIndex {
            let on = wrapper.isOn(runs[index])
            var end = index
            while end < runs.endIndex, wrapper.isOn(runs[end]) == on {
                end += 1
            }
            let inner = serialize(runs[index..<end], level: level + 1)
            output += on ? hugging(inner, with: wrapper.marker) : inner
            index = end
        }
        return output
    }

    /// Markers hug the words: `**bold** ` rather than `**bold **`, which the
    /// desk would not read as bold.
    private static func hugging(_ text: String, with marker: String) -> String {
        let leading = String(text.prefix { $0 == " " })
        let trailing = String(text.reversed().prefix { $0 == " " })
        let core = text.dropFirst(leading.count).dropLast(trailing.count)
        guard !core.isEmpty else { return text }
        return leading + marker + core + marker + trailing
    }

    private static func leaf(_ run: InlineRun) -> String {
        var text = run.text
        if run.code {
            text = hugging(text, with: "`")
        }
        switch run.link {
        case .note(let target)?:
            return text == target ? "[[\(target)]]" : "[[\(target)|\(text)]]"
        case .web(let destination)?:
            return "[\(text)](\(destination))"
        case nil:
            return text
        }
    }
}
