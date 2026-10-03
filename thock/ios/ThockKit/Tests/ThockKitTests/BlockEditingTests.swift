import XCTest
@testable import ThockKit

/// The editor's round trip (V33 §8): Markdown → blocks → attributed text →
/// blocks → Markdown. The attributed text here uses plain marker attributes
/// where the app uses fonts; the paragraph kinds and origins are the real ones.
final class BlockEditingTests: XCTestCase {
    static let note = [
        "# Weekend plans",
        "",
        "Intro paragraph with **bold** and a [[Groceries]].",
        "",
        "| item | qty |",
        "|------|-----|",
        "| eggs | 12  |",
        "",
        "```swift",
        "let x = \"**not bold**\"",
        "```",
        "",
        "<!--",
        "desk-only note",
        "-->",
        "",
        "### Small heading",
        "",
        "1. first",
        "2. second",
        "",
        "- top",
        "  - nested bullet",
        "\t- tab nested",
        "  - [ ] nested task",
        "",
        "Closing paragraph.",
    ].map { $0 + "\n" }.joined()

    // MARK: Helpers

    static let bold = NSAttributedString.Key("test.bold")
    static let italic = NSAttributedString.Key("test.italic")
    static let code = NSAttributedString.Key("test.code")
    static let strike = NSAttributedString.Key("test.strike")
    static let link = NSAttributedString.Key("test.link")

    static func style(_ kind: EditorKind, _ inline: EditorInline) -> [NSAttributedString.Key: Any] {
        var attributes: [NSAttributedString.Key: Any] = [:]
        if inline.bold { attributes[bold] = true }
        if inline.italic { attributes[italic] = true }
        if inline.code { attributes[code] = true }
        if inline.strike { attributes[strike] = true }
        switch inline.link {
        case .note(let target)?: attributes[link] = "note:" + target
        case .web(let destination)?: attributes[link] = "web:" + destination
        case nil: break
        }
        return attributes
    }

    static func inline(_ attributes: [NSAttributedString.Key: Any], _ kind: EditorKind) -> EditorInline {
        var style = EditorInline()
        style.bold = attributes[bold] != nil
        style.italic = attributes[italic] != nil
        style.code = attributes[code] != nil
        style.strike = attributes[strike] != nil
        if let value = attributes[link] as? String {
            if value.hasPrefix("note:") {
                style.link = .note(String(value.dropFirst(5)))
            } else if value.hasPrefix("web:") {
                style.link = .web(String(value.dropFirst(4)))
            }
        }
        return style.shown(as: kind)
    }

    /// Opens `markdown` in the editor, lets `edit` change the text, and
    /// returns what the editor gives back.
    func reopened(_ markdown: String, edit: (NSMutableAttributedString) throws -> Void = { _ in }) throws -> (blocks: [Block], markdown: String) {
        let editing = BlockEditing(Blocks.parse(markdown))
        let text = NSMutableAttributedString(attributedString: EditorText.attributed(editing.paragraphs, style: Self.style))
        try edit(text)
        let blocks = editing.blocks(from: EditorText.paragraphs(from: text, inline: Self.inline))
        return (blocks, EditorDocument(blocks: blocks).markdown())
    }

    struct NotFound: Error {
        var needle: String
    }

    func range(of needle: String, in text: NSAttributedString) throws -> NSRange {
        let range = (text.string as NSString).range(of: needle)
        guard range.location != NSNotFound else { throw NotFound(needle: needle) }
        return range
    }

    /// Replaces the first `old` with `new`, the way typing over it would:
    /// the new characters take the attributes of the ones they replace.
    func replace(_ old: String, with new: String, in text: NSMutableAttributedString) throws {
        let found = try range(of: old, in: text)
        text.replaceCharacters(in: found, with: new)
    }

    /// Inserts `new` right after the first `needle`, taking its attributes.
    func insert(_ new: String, after needle: String, in text: NSMutableAttributedString) throws {
        let found = try range(of: needle, in: text)
        text.replaceCharacters(in: NSRange(location: found.location + found.length, length: 0), with: new)
    }

    func paragraphRange(of needle: String, in text: NSAttributedString) throws -> NSRange {
        let found = try range(of: needle, in: text)
        return (text.string as NSString).paragraphRange(for: found)
    }

    // MARK: Untouched

    func testEveryConstructComesBackByteIdenticalWhenUntouched() throws {
        let parts = [
            "table": "| item | qty |\n|------|-----|\n| eggs | 12  |\n",
            "fence": "Before\n\n```swift\nlet x = \"**not bold**\"\n# not a heading\n```\n\nAfter\n",
            "comment": "Text\n\n<!--\n# hidden\n- [ ] hidden\n-->\n\nMore\n",
            "level 1 heading": "# One\n\nText\n",
            "level 3 heading": "### Three\n\nText\n",
            "ordered list": "1. first\n2. second\n3) third\n",
            "nested bullets": "- top\n  - nested\n\t- tab\n    - deeper\n",
            "non-canonical inline": "*star italic* and __under bold__ <!-- inline comment -->\n",
            "soft-wrapped": "First line\nsecond line\n",
            "front matter": "---\na: 1\n---\n\nText\n",
            "divider": "Above\n\n***\n\nBelow\n",
            "whole note": Self.note,
        ]
        for (name, markdown) in parts {
            let result = try reopened(markdown)
            XCTAssertEqual(result.markdown, markdown, name)
            XCTAssertFalse(result.blocks.contains { $0.touched }, name)
        }
    }

    func testTheEditorShowsOnlyWhatItCanEdit() {
        let editing = BlockEditing(Blocks.parse(Self.note))
        let kinds = editing.paragraphs.map(\.kind)
        XCTAssertEqual(kinds, [.heading, .paragraph, .heading, .bullet, .bullet, .bullet, .bullet, .bullet, .task, .paragraph])
        let text = EditorText.attributed(editing.paragraphs, style: Self.style).string
        XCTAssertFalse(text.contains("| item"))
        XCTAssertFalse(text.contains("not bold"))
        XCTAssertFalse(text.contains("desk-only"))
        XCTAssertFalse(text.contains("#"))
    }

    // MARK: Edits

    func testEditingOneParagraphLeavesEverythingElseByteIdentical() throws {
        let result = try reopened(Self.note) { text in
            try self.replace("Intro", with: "Opening", in: text)
        }
        XCTAssertEqual(result.markdown, Self.note.replacingOccurrences(of: "Intro paragraph", with: "Opening paragraph"))
        XCTAssertEqual(result.blocks.filter(\.touched).map(\.text), ["Opening paragraph with **bold** and a [[Groceries]]."])
    }

    func testAnEditKeepsHeadingLevelListMarkerAndIndent() throws {
        let result = try reopened(Self.note) { text in
            try self.replace("Small heading", with: "Smaller heading", in: text)
            try self.replace("second", with: "second item", in: text)
            try self.replace("nested bullet", with: "nested item", in: text)
            try self.replace("tab nested", with: "tab item", in: text)
            try self.replace("Weekend", with: "Long weekend", in: text)
        }
        let expected = Self.note
            .replacingOccurrences(of: "### Small heading", with: "### Smaller heading")
            .replacingOccurrences(of: "2. second", with: "2. second item")
            .replacingOccurrences(of: "  - nested bullet", with: "  - nested item")
            .replacingOccurrences(of: "\t- tab nested", with: "\t- tab item")
            .replacingOccurrences(of: "# Weekend plans", with: "# Long weekend plans")
        XCTAssertEqual(result.markdown, expected)
    }

    func testTickingANestedTaskKeepsItsIndent() throws {
        let result = try reopened(Self.note) { text in
            let range = try self.paragraphRange(of: "nested task", in: text)
            text.addAttribute(.thockKind, value: EditorKind.taskDone.rawValue, range: range)
        }
        XCTAssertEqual(result.markdown, Self.note.replacingOccurrences(of: "  - [ ] nested task", with: "  - [x] nested task"))
    }

    func testDeletingAParagraphKeepsTheBlocksAroundItWithOneBlankLine() throws {
        let result = try reopened(Self.note) { text in
            let range = try self.paragraphRange(of: "Intro paragraph", in: text)
            text.deleteCharacters(in: range)
        }
        XCTAssertEqual(result.markdown, Self.note.replacingOccurrences(of: "Intro paragraph with **bold** and a [[Groceries]].\n\n", with: ""))
    }

    func testDeletingEverythingVisibleStillCarriesTheRest() throws {
        let result = try reopened(Self.note) { text in
            text.deleteCharacters(in: NSRange(location: 0, length: text.length))
        }
        XCTAssertTrue(result.markdown.contains("| item | qty |\n|------|-----|\n| eggs | 12  |\n"))
        XCTAssertTrue(result.markdown.contains("```swift\nlet x = \"**not bold**\"\n```\n"))
        XCTAssertTrue(result.markdown.contains("<!--\ndesk-only note\n-->\n"))
        XCTAssertFalse(result.blocks.contains { EditorKind(displaying: $0.kind) != nil })
    }

    func testANewItemContinuesTheListItFollows() throws {
        let result = try reopened(Self.note) { text in
            try self.insert("\nthird", after: "second", in: text)
            try self.insert("\nsibling", after: "nested bullet", in: text)
        }
        let expected = Self.note
            .replacingOccurrences(of: "2. second\n", with: "2. second\n3. third\n")
            .replacingOccurrences(of: "  - nested bullet\n", with: "  - nested bullet\n  - sibling\n")
        XCTAssertEqual(result.markdown, expected)
    }

    func testANewParagraphIsSeparatedFromTheBlockAfterIt() throws {
        let result = try reopened("First\n\nSecond\n") { text in
            text.replaceCharacters(in: NSRange(location: 0, length: 0), with: "Zeroth\n")
        }
        // The typed line takes the first paragraph's origin; the original
        // first paragraph is then new, so the text is written in order.
        XCTAssertEqual(result.markdown, "Zeroth\n\nFirst\n\nSecond\n")
    }

    // MARK: Inline comments

    func testInlineCommentsSurviveTheEditorUnseen() throws {
        for markdown in ["Standup <!--gcal:9f2c-->\n", "- [ ] Call [[Ana]]<!--gcal:9f2c--> today\n", "See [site](https://a.test) <!-- note --> after\n"] {
            let editing = BlockEditing(Blocks.parse(markdown))
            let text = EditorText.attributed(editing.paragraphs, style: Self.style)
            XCTAssertFalse(text.string.contains("<!--"), markdown)
            XCTAssertTrue(text.string.contains(EditorText.commentMark), markdown)

            let untouched = try reopened(markdown)
            XCTAssertEqual(untouched.markdown, markdown)
            XCTAssertFalse(untouched.blocks.contains { $0.touched }, markdown)

            // Rebuilt from the paragraphs, not copied from the source, the
            // comment is still written back where it was.
            let rebuilt = BlockEditing().blocks(from: EditorText.paragraphs(from: text, inline: Self.inline))
            XCTAssertEqual(EditorDocument(blocks: rebuilt).markdown(), markdown, markdown)
        }
    }

    func testEditingNextToAnInlineCommentKeepsIt() throws {
        let result = try reopened("Standup <!--gcal:9f2c-->\n") { text in
            try self.replace("Standup", with: "Daily standup", in: text)
        }
        XCTAssertEqual(result.markdown, "Daily standup <!--gcal:9f2c-->\n")
    }

    // MARK: The inbox edit

    func inboxWrites() -> PhoneWrites {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC") ?? .current
        return PhoneWrites(config: VaultConfig(config: SampleVault.config), deviceID: "c41a9e0d7b2f4a61", now: Date(timeIntervalSince1970: 1_790_975_640), calendar: calendar)
    }

    static let inboxNote = "---\nsource:   thock-ios\ncapture:  abcdefabcdef\ncaptured: 2026-10-02T21:14:00Z\ntitle:    Weekend plans\n---\n\n" + note

    func testAWaitingNoteOpenedAndClosedWritesNothing() throws {
        let editorText = try XCTUnwrap(PhoneWrites.inboxEditorText(Self.inboxNote))
        let result = try reopened(editorText)
        XCTAssertEqual(EditorDocument(blocks: result.blocks).lines().joined(separator: "\n"), editorText)
        let edit = try XCTUnwrap(inboxWrites().inboxEdit(path: "inbox/plans.md", note: Self.inboxNote, blocks: result.blocks))
        XCTAssertTrue(edit.writes.isEmpty)
    }

    func testEditingOneParagraphOfAWaitingNoteRewritesOnlyThatParagraph() throws {
        let editorText = try XCTUnwrap(PhoneWrites.inboxEditorText(Self.inboxNote))
        let result = try reopened(editorText) { text in
            try self.replace("Closing", with: "Final", in: text)
        }
        let edit = try XCTUnwrap(inboxWrites().inboxEdit(path: "inbox/plans.md", note: Self.inboxNote, blocks: result.blocks))
        XCTAssertEqual(edit.writes.map(\.document.kind), [.replaceSection])
        var text = Self.inboxNote
        for write in edit.writes {
            text = SyncCore.apply(existing: text, write: write.document).text
        }
        XCTAssertEqual(text, Self.inboxNote.replacingOccurrences(of: "Closing paragraph.", with: "Final paragraph."))
    }
}
