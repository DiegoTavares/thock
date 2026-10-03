import SwiftUI
import ThockKit
import UIKit

/// The phone's editor (V33 §8): rich text with a deliberately small
/// vocabulary, where every style maps to one Markdown form the desk already
/// conceals. The person never sees a `#` or a `- [ ]`.
enum EditorKind: String {
    case paragraph
    case bullet
    case task
    case taskDone
    case heading
    case quote

    var isList: Bool { self == .bullet || self == .task || self == .taskDone }

    /// What a new paragraph is after pressing return in this one.
    var continued: EditorKind {
        switch self {
        case .bullet: return .bullet
        case .task, .taskDone: return .task
        default: return .paragraph
        }
    }

    var blockKind: BlockKind {
        switch self {
        case .paragraph: return .paragraph
        case .bullet: return .bullet
        case .task: return .task(checked: false)
        case .taskDone: return .task(checked: true)
        case .heading: return .heading(level: 2)
        case .quote: return .quote
        }
    }

    init(_ kind: BlockKind) {
        switch kind {
        case .bullet, .numbered: self = .bullet
        case .task(let checked): self = checked ? .taskDone : .task
        case .heading: self = .heading
        case .quote: self = .quote
        default: self = .paragraph
        }
    }
}

struct EditorInline: Equatable {
    var bold = false
    var italic = false
    var code = false
    var strike = false
    var link: LinkTarget?
}

extension NSAttributedString.Key {
    /// The one custom attribute. Inline styles live in the standard ones
    /// (font traits, strikethrough, link), because UIKit carries only those
    /// from one typed character to the next.
    static let thockKind = NSAttributedString.Key("thock.kind")
}

enum EditorStyle {
    static let markerWidth: CGFloat = 30

    static let noteScheme = "thock-note"

    static func inline(_ attributes: [NSAttributedString.Key: Any], kind: EditorKind) -> EditorInline {
        var style = EditorInline()
        if kind != .heading, let traits = (attributes[.font] as? UIFont)?.fontDescriptor.symbolicTraits {
            style.bold = traits.contains(.traitBold)
            style.italic = traits.contains(.traitItalic)
            style.code = traits.contains(.traitMonoSpace)
        }
        // A done task is struck as a whole; that is its checkbox, not a style.
        style.strike = kind != .taskDone && attributes[.strikethroughStyle] != nil
        if let url = attributes[.link] as? URL {
            if url.scheme == noteScheme {
                style.link = .note(String(url.path(percentEncoded: false).dropFirst()))
            } else {
                style.link = .web(url.absoluteString)
            }
        }
        return style
    }

    static func linkURL(_ link: LinkTarget) -> URL? {
        switch link {
        case .note(let target):
            var components = URLComponents()
            components.scheme = noteScheme
            components.path = "/" + target
            return components.url
        case .web(let destination):
            return URL(string: destination)
        }
    }

    static func attributes(kind: EditorKind, inline: EditorInline, size: CGFloat) -> [NSAttributedString.Key: Any] {
        var font: UIFont
        if kind == .heading {
            font = Theme.serifUI(size + 4, weight: 600, style: .title3)
        } else if inline.code {
            font = UIFontMetrics(forTextStyle: .body).scaledFont(for: .monospacedSystemFont(ofSize: size - 2, weight: .regular))
        } else {
            font = UIFontMetrics(forTextStyle: .body).scaledFont(for: .systemFont(ofSize: size))
        }
        var traits: UIFontDescriptor.SymbolicTraits = inline.code ? .traitMonoSpace : []
        if inline.bold { traits.insert(.traitBold) }
        if inline.italic { traits.insert(.traitItalic) }
        if inline.bold || inline.italic, kind != .heading, let descriptor = font.fontDescriptor.withSymbolicTraits(traits) {
            font = UIFont(descriptor: descriptor, size: 0)
        }

        let paragraph = NSMutableParagraphStyle()
        paragraph.lineSpacing = 3
        paragraph.paragraphSpacing = kind.isList ? 4 : 10
        let indent: CGFloat = kind.isList ? markerWidth : kind == .quote ? 16 : 0
        paragraph.firstLineHeadIndent = indent
        paragraph.headIndent = indent

        var color = kind == .quote ? Theme.mutedUI : Theme.inkUI
        if kind == .taskDone { color = Theme.dimUI }
        switch inline.link {
        case .note?: color = Theme.amberUI
        case .web?: color = Theme.calUI
        case nil: break
        }

        var attributes: [NSAttributedString.Key: Any] = [
            .font: font,
            .foregroundColor: color,
            .paragraphStyle: paragraph,
            .thockKind: kind.rawValue,
        ]
        if inline.strike || kind == .taskDone {
            attributes[.strikethroughStyle] = NSUnderlineStyle.single.rawValue
            attributes[.strikethroughColor] = Theme.dimUI
        }
        if let url = inline.link.flatMap(linkURL) {
            attributes[.link] = url
        }
        return attributes
    }

    /// The kind of the paragraph at `range`: whichever of its characters
    /// still carries it.
    static func kind(in text: NSAttributedString, paragraph range: NSRange) -> EditorKind {
        var found: EditorKind?
        text.enumerateAttribute(.thockKind, in: range) { value, _, stop in
            if let raw = value as? String, let kind = EditorKind(rawValue: raw) {
                found = kind
                stop.pointee = true
            }
        }
        return found ?? .paragraph
    }

    static func attributed(_ blocks: [Block], size: CGFloat) -> NSAttributedString {
        let output = NSMutableAttributedString()
        let editable = blocks.filter { block in
            if case .numbered = block.kind { return true }
            return block.kind.isEditable
        }
        for (index, block) in editable.enumerated() {
            let kind = EditorKind(block.kind)
            for run in block.runs {
                let inline = EditorInline(bold: run.bold, italic: run.italic, code: run.code, strike: run.strike, link: run.link)
                output.append(NSAttributedString(string: run.text, attributes: attributes(kind: kind, inline: inline, size: size)))
            }
            if index < editable.count - 1 {
                output.append(NSAttributedString(string: "\n", attributes: attributes(kind: kind, inline: EditorInline(), size: size)))
            }
        }
        return output
    }

    /// The editor's content as blocks of the subset, ready to be written.
    static func blocks(from text: NSAttributedString) -> [Block] {
        var blocks: [Block] = []
        let string = text.string as NSString
        string.enumerateSubstrings(in: NSRange(location: 0, length: string.length), options: [.byParagraphs, .substringNotRequired]) { _, range, enclosing, _ in
            guard enclosing.length > 0 else { return }
            let kind = kind(in: text, paragraph: enclosing)
            var runs: [InlineRun] = []
            if range.length > 0 {
                text.enumerateAttributes(in: range) { attributes, runRange, _ in
                    let inline = inline(attributes, kind: kind)
                    runs.append(InlineRun(string.substring(with: runRange), bold: inline.bold, italic: inline.italic, code: inline.code, strike: inline.strike, link: inline.link))
                }
            }
            let markdown = Inline.serialize(runs).trimmingCharacters(in: .whitespaces)
            if markdown.isEmpty {
                blocks.append(Block(id: blocks.count, kind: .blank, text: "", touched: true))
            } else {
                blocks.append(Block(id: blocks.count, kind: kind.blockKind, text: markdown, touched: true))
            }
        }
        return blocks
    }
}

/// Draws bullets, checkboxes and quote bars in the paragraph's indent, so the
/// text itself holds only the person's words.
final class MarkerLayoutManager: NSLayoutManager {
    var trailingKind: EditorKind = .paragraph

    override func drawBackground(forGlyphRange glyphsToShow: NSRange, at origin: CGPoint) {
        super.drawBackground(forGlyphRange: glyphsToShow, at: origin)
        guard let storage = textStorage else { return }
        let string = storage.string as NSString
        if string.length > 0 {
            let characters = characterRange(forGlyphRange: glyphsToShow, actualGlyphRange: nil)
            string.enumerateSubstrings(in: string.paragraphRange(for: characters), options: [.byParagraphs, .substringNotRequired]) { _, range, enclosing, _ in
                guard enclosing.length > 0 else { return }
                let kind = EditorStyle.kind(in: storage, paragraph: enclosing)
                guard kind != .paragraph, kind != .heading else { return }
                let glyphs = self.glyphRange(forCharacterRange: enclosing, actualCharacterRange: nil)
                let first = self.lineFragmentRect(forGlyphAt: glyphs.location, effectiveRange: nil)
                if kind == .quote {
                    var area = first
                    self.enumerateLineFragments(forGlyphRange: glyphs) { rect, _, _, _, _ in
                        area = area.union(rect)
                    }
                    Theme.ruleUI.setFill()
                    UIBezierPath(rect: CGRect(x: origin.x + 2, y: origin.y + area.minY + 2, width: 2, height: area.height - 12)).fill()
                } else {
                    let font = storage.attribute(.font, at: enclosing.location, effectiveRange: nil) as? UIFont
                    self.drawMarker(kind, in: first, origin: origin, lineHeight: font?.lineHeight ?? 22)
                }
            }
        }
        if string.length == 0 || string.hasSuffix("\n"), trailingKind.isList, extraLineFragmentRect != .zero {
            drawMarker(trailingKind, in: extraLineFragmentRect, origin: origin, lineHeight: extraLineFragmentRect.height)
        }
    }

    private func drawMarker(_ kind: EditorKind, in line: CGRect, origin: CGPoint, lineHeight: CGFloat) {
        let centre = origin.y + line.minY + min(line.height, lineHeight) / 2
        switch kind {
        case .bullet:
            Theme.mutedUI.setFill()
            UIBezierPath(ovalIn: CGRect(x: origin.x + 9, y: centre - 3, width: 6, height: 6)).fill()
        case .task, .taskDone:
            let box = CGRect(x: origin.x + 2, y: centre - 9, width: 18, height: 18)
            let path = UIBezierPath(roundedRect: box, cornerRadius: 5.5)
            if kind == .taskDone {
                Theme.amberUI.setFill()
                path.fill()
                let check = UIBezierPath()
                check.move(to: CGPoint(x: box.minX + 4.5, y: box.midY + 0.5))
                check.addLine(to: CGPoint(x: box.minX + 7.8, y: box.maxY - 5))
                check.addLine(to: CGPoint(x: box.maxX - 4.2, y: box.minY + 5.2))
                check.lineWidth = 2
                check.lineCapStyle = .round
                check.lineJoinStyle = .round
                Theme.amberInkUI.setStroke()
                check.stroke()
            } else {
                Theme.mutedUI.setStroke()
                path.lineWidth = 1.5
                path.stroke()
            }
        default:
            break
        }
    }
}

final class EditorTextView: UITextView {
    let markers = MarkerLayoutManager()
    var fontSize: CGFloat = 18
    let placeholder = UILabel()

    init(fontSize: CGFloat) {
        self.fontSize = fontSize
        let storage = NSTextStorage()
        let container = NSTextContainer(size: CGSize(width: 0, height: CGFloat.greatestFiniteMagnitude))
        container.widthTracksTextView = true
        markers.addTextContainer(container)
        storage.addLayoutManager(markers)
        super.init(frame: .zero, textContainer: container)
        backgroundColor = .clear
        textContainerInset = UIEdgeInsets(top: 4, left: 0, bottom: 8, right: 0)
        textContainer.lineFragmentPadding = 0
        tintColor = Theme.amberUI
        adjustsFontForContentSizeCategory = true
        keyboardDismissMode = .interactive
        typingAttributes = EditorStyle.attributes(kind: .paragraph, inline: EditorInline(), size: fontSize)
        // Links keep the colours the style gives them: amber for a note,
        // calendar blue for the web.
        linkTextAttributes = [:]

        placeholder.font = UIFontMetrics(forTextStyle: .body).scaledFont(for: .systemFont(ofSize: fontSize))
        placeholder.textColor = Theme.dimUI
        placeholder.numberOfLines = 0
        addSubview(placeholder)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    /// Taking the keyboard only works once the view is on screen.
    var wantsFocus = false

    override func didMoveToWindow() {
        super.didMoveToWindow()
        guard wantsFocus, window != nil else { return }
        wantsFocus = false
        DispatchQueue.main.async { [weak self] in
            self?.becomeFirstResponder()
            #if DEBUG
            self?.typeLaunchScript()
            #endif
        }
    }

    #if DEBUG
    /// `-thock-type "<keys>"` types into the first editor on screen through
    /// the same delegate path as the keyboard, then leaves the Markdown it
    /// would write in the app's temporary folder. `\n` is return, `\b` is
    /// backspace, `{B}` `{I}` `{List}` `{Task}` press the bar's buttons, `{Pick}` takes the
    /// first note suggestion.
    private func typeLaunchScript() {
        let arguments = ProcessInfo.processInfo.arguments
        guard let index = arguments.firstIndex(of: "-thock-type"), index + 1 < arguments.count else { return }
        var script = Substring(arguments[index + 1].replacingOccurrences(of: "\\n", with: "\n").replacingOccurrences(of: "\\b", with: "\u{8}"))
        while let character = script.first {
            if character == "{", let close = script.firstIndex(of: "}") {
                let name = String(script[script.index(after: script.startIndex)..<close])
                script = script[script.index(after: close)...]
                switch name {
                case "B": toggle(\.bold)
                case "I": toggle(\.italic)
                case "List": setKind(kindAtCursor == .bullet ? .paragraph : .bullet)
                case "Task": setKind(kindAtCursor == .task ? .paragraph : .task)
                case "Pick":
                    if let coordinator = delegate as? RichTextEditor.Coordinator, let link = coordinator.linkQuery(self),
                       let first = coordinator.parent.suggestions(link.query).first
                    {
                        coordinator.insertLink(first, in: self)
                    }
                default: break
                }
                continue
            }
            script = script.dropFirst()
            if character == "\u{8}" {
                let range = NSRange(location: max(selectedRange.location - 1, 0), length: selectedRange.location > 0 ? 1 : 0)
                if range.length == 0, !isInTrailingEmptyParagraph { continue }
                let probe = range.length == 0 ? NSRange(location: 0, length: 1) : range
                if delegate?.textView?(self, shouldChangeTextIn: probe, replacementText: "") ?? true, range.length > 0 {
                    deleteBackward()
                }
                continue
            }
            if delegate?.textView?(self, shouldChangeTextIn: selectedRange, replacementText: String(character)) ?? true {
                insertText(String(character))
            }
        }
        let lines = EditorDocument(blocks: EditorStyle.blocks(from: textStorage)).lines()
        try? lines.joined(separator: "\n").write(toFile: NSTemporaryDirectory() + "editor-dump.md", atomically: true, encoding: .utf8)
    }
    #endif

    override func layoutSubviews() {
        super.layoutSubviews()
        let indent: CGFloat = markers.trailingKind.isList ? EditorStyle.markerWidth : 0
        let width = bounds.width - indent
        let size = placeholder.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
        placeholder.frame = CGRect(x: indent, y: textContainerInset.top, width: width, height: size.height)
        placeholder.isHidden = textStorage.length > 0
    }

    var currentParagraph: NSRange {
        (textStorage.string as NSString).paragraphRange(for: selectedRange)
    }

    /// The cursor sits in an empty paragraph at the very end, which has no
    /// character to carry its kind.
    var isInTrailingEmptyParagraph: Bool {
        let string = textStorage.string as NSString
        return selectedRange.length == 0 && selectedRange.location == string.length && (string.length == 0 || string.hasSuffix("\n"))
    }

    var kindAtCursor: EditorKind {
        if isInTrailingEmptyParagraph { return markers.trailingKind }
        return EditorStyle.kind(in: textStorage, paragraph: currentParagraph)
    }

    var inlineAtCursor: EditorInline {
        EditorStyle.inline(typingAttributes, kind: kindAtCursor)
    }

    /// UIKit drops custom attributes from what it types next, so after an
    /// edit each paragraph's kind is spread back over all of it.
    func spreadKinds() {
        guard markedTextRange == nil, textStorage.length > 0 else { return }
        let string = textStorage.string as NSString
        var fixes: [(NSRange, EditorKind)] = []
        string.enumerateSubstrings(in: NSRange(location: 0, length: string.length), options: [.byParagraphs, .substringNotRequired]) { _, _, enclosing, _ in
            guard enclosing.length > 0 else { return }
            var covered = true
            self.textStorage.enumerateAttribute(.thockKind, in: enclosing) { value, _, stop in
                if value == nil {
                    covered = false
                    stop.pointee = true
                }
            }
            if !covered {
                fixes.append((enclosing, EditorStyle.kind(in: self.textStorage, paragraph: enclosing)))
            }
        }
        guard !fixes.isEmpty else { return }
        let selection = selectedRange
        let typing = typingAttributes
        textStorage.beginEditing()
        for (range, kind) in fixes {
            textStorage.addAttribute(.thockKind, value: kind.rawValue, range: range)
        }
        textStorage.endEditing()
        selectedRange = selection
        typingAttributes = typing
    }

    func setKind(_ kind: EditorKind) {
        if isInTrailingEmptyParagraph {
            markers.trailingKind = kind
            typingAttributes = EditorStyle.attributes(kind: kind, inline: inlineAtCursor, size: fontSize)
            markers.invalidateDisplay(forCharacterRange: NSRange(location: 0, length: textStorage.length))
            setNeedsDisplay()
            setNeedsLayout()
            return
        }
        let string = textStorage.string as NSString
        let range = string.paragraphRange(for: selectedRange)
        let inline = inlineAtCursor
        restyle(range, kind: kind)
        typingAttributes = EditorStyle.attributes(kind: kind, inline: inline, size: fontSize)
    }

    /// Rebuilds the display attributes of `range` from each run's own inline
    /// style, optionally under a new paragraph kind.
    func restyle(_ range: NSRange, kind: EditorKind? = nil, change: ((inout EditorInline) -> Void)? = nil) {
        guard range.length > 0 else { return }
        let selection = selectedRange
        let string = textStorage.string as NSString
        var updates: [(NSRange, [NSAttributedString.Key: Any])] = []
        textStorage.enumerateAttributes(in: range) { attributes, runRange, _ in
            let current = EditorStyle.kind(in: textStorage, paragraph: string.paragraphRange(for: NSRange(location: runRange.location, length: 0)))
            var inline = EditorStyle.inline(attributes, kind: current)
            change?(&inline)
            updates.append((runRange, EditorStyle.attributes(kind: kind ?? current, inline: inline, size: fontSize)))
        }
        textStorage.beginEditing()
        for (runRange, attributes) in updates {
            textStorage.setAttributes(attributes, range: runRange)
        }
        textStorage.endEditing()
        selectedRange = selection
    }

    func toggle(_ keyPath: WritableKeyPath<EditorInline, Bool>) {
        if selectedRange.length > 0 {
            var allOn = true
            let kind = kindAtCursor
            textStorage.enumerateAttributes(in: selectedRange) { attributes, _, _ in
                if !EditorStyle.inline(attributes, kind: kind)[keyPath: keyPath] { allOn = false }
            }
            restyle(selectedRange) { $0[keyPath: keyPath] = !allOn }
        } else {
            var inline = inlineAtCursor
            inline[keyPath: keyPath].toggle()
            typingAttributes = EditorStyle.attributes(kind: kindAtCursor, inline: inline, size: fontSize)
        }
    }
}

/// Lets a screen ask the editor for its content, or give it the keyboard.
final class EditorHandle {
    fileprivate weak var textView: EditorTextView?

    var blocks: [Block] {
        guard let textView else { return [] }
        return EditorStyle.blocks(from: textView.attributedText)
    }

    var isEmpty: Bool {
        textView?.textStorage.string.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true
    }

    func focus() {
        textView?.becomeFirstResponder()
    }

    func resign() {
        textView?.resignFirstResponder()
    }
}

struct RichTextEditor: UIViewRepresentable {
    var initial: [Block] = []
    var handle: EditorHandle
    var placeholder = ""
    var fontSize: CGFloat = 18
    var scrolls = true
    var autofocus = true
    var suggestions: (String) -> [String] = { _ in [] }
    var onChange: ([Block]) -> Void = { _ in }

    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }

    func makeUIView(context: Context) -> EditorTextView {
        let view = EditorTextView(fontSize: fontSize)
        view.delegate = context.coordinator
        view.isScrollEnabled = scrolls
        view.placeholder.text = placeholder
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        if !initial.isEmpty {
            view.attributedText = EditorStyle.attributed(initial, size: fontSize)
            view.selectedRange = NSRange(location: view.textStorage.length, length: 0)
        }
        let bar = FormatBar(textView: view, coordinator: context.coordinator)
        view.inputAccessoryView = bar
        context.coordinator.bar = bar
        let tap = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.tapped(_:)))
        tap.delegate = context.coordinator
        view.addGestureRecognizer(tap)
        handle.textView = view
        view.wantsFocus = autofocus
        return view
    }

    func updateUIView(_ view: EditorTextView, context: Context) {
        context.coordinator.parent = self
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: EditorTextView, context: Context) -> CGSize? {
        guard !scrolls, let width = proposal.width, width.isFinite else { return nil }
        let size = uiView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
        return CGSize(width: width, height: max(size.height, 40))
    }

    final class Coordinator: NSObject, UITextViewDelegate, UIGestureRecognizerDelegate {
        var parent: RichTextEditor
        weak var bar: FormatBar?

        init(_ parent: RichTextEditor) {
            self.parent = parent
        }

        func textViewDidChange(_ textView: UITextView) {
            guard let view = textView as? EditorTextView else { return }
            let string = view.textStorage.string as NSString
            if string.length > 0, !string.hasSuffix("\n") {
                view.markers.trailingKind = .paragraph
            }
            view.spreadKinds()
            view.setNeedsLayout()
            view.invalidateIntrinsicContentSize()
            updateSuggestions(view)
            parent.onChange(EditorStyle.blocks(from: view.textStorage))
        }

        func textViewDidChangeSelection(_ textView: UITextView) {
            guard let view = textView as? EditorTextView, view.selectedRange.length == 0 else { return }
            // UIKit takes typing attributes from the character before the
            // cursor, which after a return is the previous paragraph's.
            let paragraph = view.currentParagraph
            let kind = view.kindAtCursor
            var inline = view.inlineAtCursor
            // A link ends where its text ends; typing after it is plain.
            inline.link = nil
            if paragraph.length > 0, view.selectedRange.location == paragraph.location {
                let string = view.textStorage.string as NSString
                let first = string.substring(with: NSRange(location: paragraph.location, length: 1))
                inline = first == "\n" ? EditorInline() : EditorStyle.inline(view.textStorage.attributes(at: paragraph.location, effectiveRange: nil), kind: kind).withoutLink
            }
            view.typingAttributes = EditorStyle.attributes(kind: kind, inline: inline, size: view.fontSize)
            bar?.refresh()
        }

        func textView(_ textView: UITextView, shouldChangeTextIn range: NSRange, replacementText text: String) -> Bool {
            guard let view = textView as? EditorTextView else { return true }
            let string = view.textStorage.string as NSString
            let paragraph = string.paragraphRange(for: NSRange(location: range.location, length: 0))
            let kind = view.kindAtCursor
            let before = string.substring(with: NSRange(location: paragraph.location, length: range.location - paragraph.location))

            if text == "\n", range.length == 0 {
                let content = string.substring(with: paragraph).trimmingCharacters(in: .whitespacesAndNewlines)
                if kind != .paragraph, content.isEmpty {
                    // Return on an empty list item ends the list.
                    view.setKind(.paragraph)
                    return false
                }
                let attributes = EditorStyle.attributes(kind: kind, inline: EditorInline(), size: view.fontSize)
                view.textStorage.replaceCharacters(in: range, with: NSAttributedString(string: "\n", attributes: attributes))
                view.selectedRange = NSRange(location: range.location + 1, length: 0)
                let next = kind.continued
                let rest = (view.textStorage.string as NSString).paragraphRange(for: view.selectedRange)
                if view.isInTrailingEmptyParagraph {
                    view.markers.trailingKind = next
                } else if rest.length > 0 {
                    view.restyle(rest, kind: next)
                }
                view.typingAttributes = EditorStyle.attributes(kind: next, inline: EditorInline(), size: view.fontSize)
                textViewDidChange(view)
                view.scrollRangeToVisible(view.selectedRange)
                return false
            }

            if text.isEmpty, range.length == 1, view.selectedRange.length == 0, kind != .paragraph {
                // Backspace at the start of a list item removes the marker,
                // not the line break before it.
                let atStart = view.isInTrailingEmptyParagraph || view.selectedRange.location == view.currentParagraph.location
                if atStart {
                    view.setKind(.paragraph)
                    return false
                }
            }

            if text == " ", range.length == 0 {
                // People who know Markdown can still type it.
                let shortcut: EditorKind?
                switch before {
                case "-", "*": shortcut = .bullet
                case "[]", "[ ]", "- []", "- [ ]": shortcut = .task
                case ">": shortcut = .quote
                case "#", "##": shortcut = .heading
                default: shortcut = kind == .bullet && (before == "[]" || before == "[ ]") ? .task : nil
                }
                if let shortcut {
                    view.textStorage.replaceCharacters(in: NSRange(location: paragraph.location, length: before.utf16.count), with: "")
                    view.selectedRange = NSRange(location: paragraph.location, length: 0)
                    if (view.textStorage.string as NSString).paragraphRange(for: view.selectedRange).length == 0 || view.isInTrailingEmptyParagraph {
                        view.markers.trailingKind = shortcut
                    }
                    view.setKind(shortcut)
                    textViewDidChange(view)
                    return false
                }
            }
            return true
        }

        // MARK: Checkbox taps

        func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer, shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool {
            true
        }

        @objc func tapped(_ recognizer: UITapGestureRecognizer) {
            guard let view = recognizer.view as? EditorTextView, view.textStorage.length > 0 else { return }
            let point = recognizer.location(in: view)
            guard point.x < view.textContainerInset.left + EditorStyle.markerWidth else { return }
            let location = CGPoint(x: EditorStyle.markerWidth + 1, y: point.y - view.textContainerInset.top)
            let glyph = view.markers.glyphIndex(for: location, in: view.textContainer)
            let character = view.markers.characterIndexForGlyph(at: glyph)
            let paragraph = (view.textStorage.string as NSString).paragraphRange(for: NSRange(location: character, length: 0))
            guard paragraph.length > 0 else { return }
            let kind = EditorStyle.kind(in: view.textStorage, paragraph: paragraph)
            guard kind == .task || kind == .taskDone else { return }
            view.restyle(paragraph, kind: kind == .task ? .taskDone : .task)
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
            textViewDidChange(view)
        }

        // MARK: Note links

        /// The text after an unclosed `[[` before the cursor, if any.
        func linkQuery(_ view: EditorTextView) -> (range: NSRange, query: String)? {
            guard view.selectedRange.length == 0 else { return nil }
            let string = view.textStorage.string as NSString
            let paragraph = view.currentParagraph
            let before = string.substring(with: NSRange(location: paragraph.location, length: view.selectedRange.location - paragraph.location)) as NSString
            let open = before.range(of: "[[", options: .backwards)
            guard open.location != NSNotFound else { return nil }
            let query = before.substring(from: open.location + 2)
            guard !query.contains("]"), !query.contains("["), query.count <= 40 else { return nil }
            return (NSRange(location: paragraph.location + open.location, length: before.length - open.location), query)
        }

        func updateSuggestions(_ view: EditorTextView) {
            guard let link = linkQuery(view) else {
                bar?.show(suggestions: nil)
                return
            }
            bar?.show(suggestions: parent.suggestions(link.query))
        }

        func insertLink(_ target: String, in view: EditorTextView) {
            guard let link = linkQuery(view) else { return }
            let kind = view.kindAtCursor
            let label = target.split(separator: "/").last.map(String.init) ?? target
            let linked = NSMutableAttributedString(string: label, attributes: EditorStyle.attributes(kind: kind, inline: EditorInline(link: .note(target)), size: view.fontSize))
            linked.append(NSAttributedString(string: " ", attributes: EditorStyle.attributes(kind: kind, inline: EditorInline(), size: view.fontSize)))
            view.textStorage.replaceCharacters(in: link.range, with: linked)
            view.selectedRange = NSRange(location: link.range.location + linked.length, length: 0)
            view.typingAttributes = EditorStyle.attributes(kind: kind, inline: EditorInline(), size: view.fontSize)
            textViewDidChange(view)
        }
    }
}

extension EditorInline {
    var withoutLink: EditorInline {
        var copy = self
        copy.link = nil
        return copy
    }
}

/// The bar above the keyboard: bold, italic, list, task, link. Heading and
/// quote hide behind a long press, because a capture rarely needs them.
final class FormatBar: UIInputView {
    private weak var textView: EditorTextView?
    private weak var coordinator: RichTextEditor.Coordinator?
    private let stack = UIStackView()
    private let scroll = UIScrollView()
    private let chips = UIStackView()
    private var showingMore = false
    private var buttons: [String: UIButton] = [:]

    init(textView: EditorTextView, coordinator: RichTextEditor.Coordinator) {
        self.textView = textView
        self.coordinator = coordinator
        super.init(frame: CGRect(x: 0, y: 0, width: 320, height: 50), inputViewStyle: .keyboard)
        allowsSelfSizing = false
        backgroundColor = Theme.surfaceUI

        let rule = UIView()
        rule.backgroundColor = Theme.ruleSoftUI
        rule.translatesAutoresizingMaskIntoConstraints = false
        addSubview(rule)

        stack.axis = .horizontal
        stack.distribution = .fillEqually
        stack.spacing = 6
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)

        scroll.showsHorizontalScrollIndicator = false
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.isHidden = true
        addSubview(scroll)
        chips.axis = .horizontal
        chips.spacing = 6
        chips.translatesAutoresizingMaskIntoConstraints = false
        scroll.addSubview(chips)

        NSLayoutConstraint.activate([
            rule.topAnchor.constraint(equalTo: topAnchor),
            rule.leadingAnchor.constraint(equalTo: leadingAnchor),
            rule.trailingAnchor.constraint(equalTo: trailingAnchor),
            rule.heightAnchor.constraint(equalToConstant: 1),
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            stack.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -8),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            scroll.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -8),
            chips.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor),
            chips.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor),
            chips.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor),
            chips.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor),
            chips.heightAnchor.constraint(equalTo: scroll.frameLayoutGuide.heightAnchor),
        ])
        addGestureRecognizer(UILongPressGestureRecognizer(target: self, action: #selector(longPressed(_:))))
        rebuild()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    private func button(_ title: String, font: UIFont, label: String, action: @escaping () -> Void) -> UIButton {
        var configuration = UIButton.Configuration.plain()
        configuration.attributedTitle = AttributedString(title, attributes: AttributeContainer([.font: font]))
        configuration.contentInsets = NSDirectionalEdgeInsets(top: 0, leading: 10, bottom: 0, trailing: 10)
        configuration.background.backgroundColor = Theme.sunkenUI
        configuration.background.cornerRadius = 9
        configuration.baseForegroundColor = Theme.mutedUI
        let button = UIButton(configuration: configuration, primaryAction: UIAction { [weak self] _ in
            action()
            self?.refresh()
        })
        button.accessibilityLabel = label
        buttons[label] = button
        return button
    }

    private func rebuild() {
        stack.arrangedSubviews.forEach { $0.removeFromSuperview() }
        buttons = [:]
        let regular = UIFont.systemFont(ofSize: 15, weight: .medium)
        if showingMore {
            stack.addArrangedSubview(button("Heading", font: Theme.serifUI(16, weight: 600), label: "Heading") { [weak self] in self?.toggleKind(.heading) })
            stack.addArrangedSubview(button("Quote", font: regular, label: "Quote") { [weak self] in self?.toggleKind(.quote) })
            stack.addArrangedSubview(button("Back", font: regular, label: "Back to formatting") { [weak self] in
                self?.showingMore = false
                self?.rebuild()
            })
        } else {
            stack.addArrangedSubview(button("B", font: .systemFont(ofSize: 16, weight: .bold), label: "Bold") { [weak self] in self?.textView?.toggle(\.bold) })
            stack.addArrangedSubview(button("I", font: .italicSystemFont(ofSize: 16), label: "Italic") { [weak self] in self?.textView?.toggle(\.italic) })
            stack.addArrangedSubview(button("• List", font: regular, label: "List") { [weak self] in self?.toggleKind(.bullet) })
            stack.addArrangedSubview(button("☐ Task", font: regular, label: "Task") { [weak self] in self?.toggleKind(.task) })
            stack.addArrangedSubview(button("[[ Link", font: regular, label: "Link to a note") { [weak self] in self?.startLink() })
        }
        refresh()
    }

    private func toggleKind(_ kind: EditorKind) {
        guard let textView else { return }
        let current = textView.kindAtCursor
        let same = current == kind || (kind == .task && current == .taskDone)
        textView.setKind(same ? .paragraph : kind)
        coordinator?.textViewDidChange(textView)
    }

    private func startLink() {
        guard let textView else { return }
        textView.insertText("[[")
    }

    @objc private func longPressed(_ recognizer: UILongPressGestureRecognizer) {
        guard recognizer.state == .began, scroll.isHidden else { return }
        showingMore.toggle()
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        rebuild()
    }

    /// Lights the buttons that describe the text at the cursor.
    func refresh() {
        guard let textView else { return }
        let inline = textView.inlineAtCursor
        let kind = textView.kindAtCursor
        let active: [String: Bool] = [
            "Bold": inline.bold, "Italic": inline.italic, "List": kind == .bullet,
            "Task": kind == .task || kind == .taskDone, "Heading": kind == .heading, "Quote": kind == .quote,
        ]
        for (label, button) in buttons {
            let on = active[label] ?? false
            button.configuration?.background.backgroundColor = on ? Theme.amberSoftUI : Theme.sunkenUI
            button.configuration?.baseForegroundColor = on ? Theme.amberUI : Theme.mutedUI
            button.accessibilityTraits = on ? [.button, .selected] : .button
        }
    }

    /// While a `[[` is open the bar offers note names instead of formatting.
    func show(suggestions: [String]?) {
        guard let suggestions, !suggestions.isEmpty else {
            scroll.isHidden = true
            stack.isHidden = false
            return
        }
        chips.arrangedSubviews.forEach { $0.removeFromSuperview() }
        for name in suggestions {
            var configuration = UIButton.Configuration.plain()
            configuration.attributedTitle = AttributedString(name, attributes: AttributeContainer([.font: UIFont.systemFont(ofSize: 15, weight: .medium)]))
            configuration.contentInsets = NSDirectionalEdgeInsets(top: 0, leading: 12, bottom: 0, trailing: 12)
            configuration.background.backgroundColor = Theme.sunkenUI
            configuration.background.cornerRadius = 9
            configuration.baseForegroundColor = Theme.amberUI
            chips.addArrangedSubview(UIButton(configuration: configuration, primaryAction: UIAction { [weak self] _ in
                guard let self, let textView = self.textView else { return }
                self.coordinator?.insertLink(name, in: textView)
            }))
        }
        scroll.isHidden = false
        stack.isHidden = true
    }
}
