import Foundation

/// A paragraph style of the phone's editor (V33 §8). Each maps to one
/// Markdown form the desk already conceals.
public enum EditorKind: String, Sendable {
    case paragraph
    case bullet
    case task
    case taskDone
    case heading
    case quote

    /// How a block is shown in the editor, or `nil` when the editor cannot
    /// show it and carries it through unseen instead.
    public init?(displaying kind: BlockKind) {
        switch kind {
        case .paragraph: self = .paragraph
        case .bullet, .numbered: self = .bullet
        case .task(let checked): self = checked ? .taskDone : .task
        case .heading: self = .heading
        case .quote: self = .quote
        case .rule, .blank, .opaque: return nil
        }
    }

    public var isList: Bool { self == .bullet || self == .task || self == .taskDone }

    /// What a new paragraph is after pressing return in this one.
    public var continued: EditorKind {
        switch self {
        case .bullet: return .bullet
        case .task, .taskDone: return .task
        default: return .paragraph
        }
    }

    /// The block a paragraph of this kind is written as when it has no
    /// block of its own to keep the level or marker from.
    public var blockKind: BlockKind {
        switch self {
        case .paragraph: return .paragraph
        case .bullet: return .bullet
        case .task: return .task(checked: false)
        case .taskDone: return .task(checked: true)
        case .heading: return .heading(level: 2)
        case .quote: return .quote
        }
    }
}

/// The inline styles of one stretch of editor text.
public struct EditorInline: Equatable, Sendable {
    public var bold = false
    public var italic = false
    public var code = false
    public var strike = false
    public var link: LinkTarget?

    public init(bold: Bool = false, italic: Bool = false, code: Bool = false, strike: Bool = false, link: LinkTarget? = nil) {
        self.bold = bold
        self.italic = italic
        self.code = code
        self.strike = strike
        self.link = link
    }

    public init(_ run: InlineRun) {
        self.init(bold: run.bold, italic: run.italic, code: run.code, strike: run.strike, link: run.link)
    }

    /// What the editor can show under `kind`: a heading has one weight, and
    /// a done task is struck as a whole, which is its checkbox, not a style.
    public func shown(as kind: EditorKind) -> EditorInline {
        var copy = self
        if kind == .heading {
            copy.bold = false
            copy.italic = false
            copy.code = false
        }
        if kind == .taskDone {
            copy.strike = false
        }
        return copy
    }
}

/// One paragraph of the editor's text.
public struct EditorParagraph: Equatable, Sendable {
    public var kind: EditorKind
    public var runs: [InlineRun]
    /// The index, in `BlockEditing.original`, of the block this paragraph was
    /// opened from; `nil` for a paragraph the person added.
    public var origin: Int?

    public init(kind: EditorKind, runs: [InlineRun], origin: Int? = nil) {
        self.kind = kind
        self.runs = runs
        self.origin = origin
    }

    /// The paragraph's text as inline Markdown, as the editor would write it.
    public var markdown: String {
        let shown = runs.map { run -> InlineRun in
            let style = EditorInline(run).shown(as: kind)
            return InlineRun(run.text, bold: style.bold, italic: style.italic, code: style.code, strike: style.strike, link: style.link)
        }
        return Inline.serialize(shown).trimmingCharacters(in: .whitespaces)
    }
}

/// The blocks a note was opened with, and how the editor's paragraphs are
/// written back over them (V33 §8, round-tripping). A paragraph the person
/// did not change is its original block, byte for byte. A changed one keeps
/// its heading level, list marker and indent. Blocks the editor cannot show
/// (tables, code, comments, front matter, dividers, blank lines) are carried
/// through unseen, each in front of the shown block that followed it.
public struct BlockEditing: Equatable, Sendable {
    public let original: [Block]

    public init(_ original: [Block] = []) {
        self.original = original
    }

    /// The paragraphs the editor opens with.
    public var paragraphs: [EditorParagraph] {
        original.enumerated().compactMap { index, block in
            guard let kind = EditorKind(displaying: block.kind) else { return nil }
            return EditorParagraph(kind: kind, runs: block.runs, origin: index)
        }
    }

    /// The editor's content as blocks, ready for `EditorDocument`. Empty
    /// paragraphs are left out; carried blocks are not.
    public func blocks(from paragraphs: [EditorParagraph]) -> [Block] {
        var output: [Block] = []
        // The original index of each output block, for carried blank lines.
        var outputOrigins: [Int?] = []
        let carried = original.indices.filter { EditorKind(displaying: original[$0].kind) == nil }
        var nextCarried = 0
        var lastClaimed = -1

        func emitCarried(before limit: Int) {
            while nextCarried < carried.count, carried[nextCarried] < limit {
                let index = carried[nextCarried]
                nextCarried += 1
                // Deleting a paragraph would otherwise leave the blank lines
                // on both sides of it next to each other.
                if original[index].kind == .blank, output.last?.kind == .blank, let previous = outputOrigins.last ?? nil, previous != index - 1 {
                    continue
                }
                output.append(original[index])
                outputOrigins.append(index)
            }
        }

        for paragraph in paragraphs {
            let markdown = paragraph.markdown
            // A paragraph split in two leaves both halves with the same
            // origin; the first keeps it and the second is new.
            if let origin = paragraph.origin, origin > lastClaimed, original.indices.contains(origin), let shown = EditorKind(displaying: original[origin].kind) {
                lastClaimed = origin
                emitCarried(before: origin)
                guard !markdown.isEmpty else { continue }
                output.append(written(paragraph, markdown: markdown, over: original[origin], shown: shown, origin: origin))
                outputOrigins.append(origin)
            } else if !markdown.isEmpty {
                output.append(added(paragraph, markdown: markdown, after: output.last))
                outputOrigins.append(nil)
            }
        }
        emitCarried(before: original.count)

        for index in output.indices {
            output[index].id = index
        }
        return output
    }

    private func written(_ paragraph: EditorParagraph, markdown: String, over block: Block, shown: EditorKind, origin: Int) -> Block {
        if paragraph.kind == shown, markdown == EditorParagraph(kind: shown, runs: block.runs, origin: origin).markdown {
            return block
        }
        var block = block
        block.text = markdown
        block.touched = true
        if paragraph.kind != shown {
            block.kind = paragraph.kind.blockKind
            if !paragraph.kind.isList {
                block.indent = ""
            }
        }
        return block
    }

    /// A new list item continues the item before it: same indent, and the
    /// next number in a numbered list.
    private func added(_ paragraph: EditorParagraph, markdown: String, after previous: Block?) -> Block {
        var block = Block(id: 0, kind: paragraph.kind.blockKind, text: markdown, touched: true)
        guard paragraph.kind.isList, let previous else { return block }
        switch previous.kind {
        case .numbered(let marker):
            block.indent = previous.indent
            if paragraph.kind == .bullet {
                block.kind = .numbered(marker: Self.marker(after: marker))
            }
        case .bullet, .task:
            block.indent = previous.indent
        default:
            break
        }
        return block
    }

    static func marker(after marker: String) -> String {
        let digits = marker.prefix { $0.isASCII && $0.isNumber }
        guard let number = Int(digits) else { return marker }
        return String(number + 1) + marker.dropFirst(digits.count)
    }
}

extension NSAttributedString.Key {
    /// The paragraph's `EditorKind`. Inline styles live in the standard
    /// attributes (font traits, strikethrough, link), because UIKit carries
    /// only those from one typed character to the next.
    public static let thockKind = NSAttributedString.Key("thock.kind")
    /// The paragraph's `EditorParagraph.origin`.
    public static let thockOrigin = NSAttributedString.Key("thock.origin")
}

/// Editor paragraphs as attributed text and back. How a style looks (fonts,
/// colours) is the caller's: `style` gives the attributes for a kind and
/// inline style, and `inline` reads the inline style back from them.
public enum EditorText {
    /// The text the editor opens with. Every character carries its
    /// paragraph's kind and origin, so either survives as long as one
    /// character of the paragraph does.
    public static func attributed(_ paragraphs: [EditorParagraph], style: @escaping (EditorKind, EditorInline) -> [NSAttributedString.Key: Any]) -> NSAttributedString {
        let output = NSMutableAttributedString()
        func paragraphAttributes(_ paragraph: EditorParagraph, _ inline: EditorInline) -> [NSAttributedString.Key: Any] {
            var attributes = style(paragraph.kind, inline)
            attributes[.thockKind] = paragraph.kind.rawValue
            if let origin = paragraph.origin {
                attributes[.thockOrigin] = origin
            }
            return attributes
        }
        for (index, paragraph) in paragraphs.enumerated() {
            for run in paragraph.runs {
                output.append(NSAttributedString(string: run.text, attributes: paragraphAttributes(paragraph, EditorInline(run).shown(as: paragraph.kind))))
            }
            if index < paragraphs.count - 1 {
                output.append(NSAttributedString(string: "\n", attributes: paragraphAttributes(paragraph, EditorInline())))
            }
        }
        return output
    }

    /// The kind of the paragraph at `range`: whichever of its characters
    /// still carries it.
    public static func kind(in text: NSAttributedString, paragraph range: NSRange) -> EditorKind {
        var found: EditorKind?
        text.enumerateAttribute(.thockKind, in: range) { value, _, stop in
            if let raw = value as? String, let kind = EditorKind(rawValue: raw) {
                found = kind
                stop.pointee = true
            }
        }
        return found ?? .paragraph
    }

    /// The origin of the paragraph at `range`, read like its kind.
    public static func origin(in text: NSAttributedString, paragraph range: NSRange) -> Int? {
        var found: Int?
        text.enumerateAttribute(.thockOrigin, in: range) { value, _, stop in
            if let origin = value as? Int {
                found = origin
                stop.pointee = true
            }
        }
        return found
    }

    /// The editor's text as paragraphs, for `BlockEditing.blocks(from:)`.
    public static func paragraphs(from text: NSAttributedString, inline: @escaping ([NSAttributedString.Key: Any], EditorKind) -> EditorInline) -> [EditorParagraph] {
        let string = text.string as NSString
        var ranges: [(content: NSRange, enclosing: NSRange)] = []
        string.enumerateSubstrings(in: NSRange(location: 0, length: string.length), options: [.byParagraphs, .substringNotRequired]) { _, range, enclosing, _ in
            if enclosing.length > 0 {
                ranges.append((range, enclosing))
            }
        }
        return ranges.map { entry -> EditorParagraph in
            let kind = EditorText.kind(in: text, paragraph: entry.enclosing)
            var runs: [InlineRun] = []
            if entry.content.length > 0 {
                text.enumerateAttributes(in: entry.content) { attributes, runRange, _ in
                    let style = inline(attributes, kind)
                    runs.append(InlineRun(string.substring(with: runRange), bold: style.bold, italic: style.italic, code: style.code, strike: style.strike, link: style.link))
                }
            }
            return EditorParagraph(kind: kind, runs: runs, origin: EditorText.origin(in: text, paragraph: entry.enclosing))
        }
    }
}
