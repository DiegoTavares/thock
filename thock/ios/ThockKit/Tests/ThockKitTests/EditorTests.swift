import XCTest
@testable import ThockKit

final class EditorTests: XCTestCase {
    /// Notes chosen for the constructs the editor must carry through untouched.
    let notes: [String: String] = [
        "frontmatter": "---\ntitle: A\ntags: [x, y]\n---\n\n# Title\n\nBody\n",
        "frontmatter closed by dots": "---\na: 1\n...\nText\n",
        "unclosed frontmatter": "---\nnot: frontmatter\n\nText\n",
        "backtick fence": "Before\n```swift\nlet x = \"**not bold**\"\n# not a heading\n```\nAfter\n",
        "tilde fence": "~~~\n- [ ] not a task\n~~~\n",
        "longer closing fence": "````\n```\ninner\n```\n````\nAfter\n",
        "unclosed fence": "Text\n```\ncode runs to the end\n- [ ] still code\n",
        "comment": "<!-- one line -->\nText\n",
        "multi-line comment": "<!--\n# hidden\n- [ ] hidden\n-->\nText\n",
        "unclosed comment": "Text\n\n<!-- never closed\nmore\n",
        "table": "| a | b |\n|---|---|\n| 1 | 2 |\nAfter the table\n",
        "raw html": "<details>\n<summary>More</summary>\n\nInside\n\n</details>\n",
        "quote": "> quoted\n>\n> second paragraph\nnot quoted\n",
        "nested lists": "- a\n  - b\n\t- tab\n    1. one\n    2) two\n- [ ] task\n  - [x] done\n- [X] upper\n",
        "rules": "***\n---\n___\n- - -\n",
        "headings": "# One\n## Two ##\n###### Six\n####### Seven is text\n#NoSpace\n",
        "soft-wrapped paragraph": "First line\nsecond line  \nthird line\n\nNext\n",
        "trailing whitespace": "Line   \n   \n\t\nEnd\t\n",
        "no final newline": "# A\n\nLast line",
        "crlf": "# A\r\n\r\n- [ ] Task\r\nText\r\n",
        "unicode": "# Café ☕️\n\n- [ ] Ligar à Ana 👩‍👩‍👧\nNFD: Cafe\u{301}\n",
        "byte order mark": "\u{FEFF}# Title\n\nText\n",
        "only blanks": "\n\n\n",
        "empty": "",
    ]

    func testEveryBlockKeepsItsSourceLinesWhenUntouched() {
        for (name, text) in notes {
            let lines = TextFile(text).lines.map(\.text)
            let document = EditorDocument(markdown: text)
            XCTAssertEqual(document.lines(), lines, name)
            XCTAssertEqual(document.blocks.flatMap(\.source), lines, name)
            XCTAssertEqual(EditorDocument(lines: lines).lines(), lines, name)
            for (index, block) in document.blocks.enumerated() {
                XCTAssertEqual(block.id, index, name)
                if index > 0 {
                    let previous = document.blocks[index - 1]
                    XCTAssertEqual(block.line, previous.line + previous.source.count, name)
                }
            }
        }
    }

    func testOpaqueBlocksAreRecognised() {
        func opaque(_ text: String) -> [[String]] {
            Blocks.parse(text).filter { $0.kind == .opaque }.map(\.source)
        }
        XCTAssertEqual(opaque(notes["frontmatter"]!), [["---", "title: A", "tags: [x, y]", "---"]])
        XCTAssertEqual(opaque(notes["backtick fence"]!), [["```swift", "let x = \"**not bold**\"", "# not a heading", "```"]])
        XCTAssertEqual(opaque(notes["longer closing fence"]!), [["````", "```", "inner", "```", "````"]])
        XCTAssertEqual(opaque(notes["unclosed fence"]!), [["```", "code runs to the end", "- [ ] still code"]])
        XCTAssertEqual(opaque(notes["multi-line comment"]!), [["<!--", "# hidden", "- [ ] hidden", "-->"]])
        XCTAssertEqual(opaque(notes["unclosed comment"]!), [["<!-- never closed", "more"]])
        XCTAssertEqual(opaque(notes["table"]!), [["| a | b |", "|---|---|", "| 1 | 2 |"]])
        XCTAssertEqual(opaque(notes["raw html"]!), [["<details>", "<summary>More</summary>"], ["</details>"]])
        XCTAssertTrue(opaque(notes["unclosed frontmatter"]!).isEmpty)
        XCTAssertFalse(Blocks.parse("- [ ] a\n```\n- [ ] b\n```\n").contains { $0.kind == .task(checked: false) && $0.text == "b" })
    }

    func testOpaqueBlocksSurviveBeingTouched() {
        for (name, text) in notes {
            var document = EditorDocument(markdown: text)
            let before = document.lines()
            for index in document.blocks.indices where document.blocks[index].kind == .opaque {
                document.blocks[index].touched = true
                document.blocks[index].text = "ignored"
            }
            XCTAssertEqual(document.lines(), before, name)
        }
    }

    func testBlockKindsAndText() {
        let blocks = Blocks.parse(notes["nested lists"]!)
        XCTAssertEqual(blocks.map(\.kind), [.bullet, .bullet, .bullet, .numbered(marker: "1."), .numbered(marker: "2)"), .task(checked: false), .task(checked: true), .task(checked: true)])
        XCTAssertEqual(blocks.map(\.indent), ["", "  ", "\t", "    ", "    ", "", "  ", ""])
        XCTAssertEqual(blocks.map(\.text), ["a", "b", "tab", "one", "two", "task", "done", "upper"])

        let headings = Blocks.parse(notes["headings"]!)
        XCTAssertEqual(headings.map(\.kind), [.heading(level: 1), .heading(level: 2), .heading(level: 6), .paragraph])
        XCTAssertEqual(headings.last?.text, "####### Seven is text\n#NoSpace")

        XCTAssertEqual(Blocks.parse(notes["quote"]!).map(\.kind), [.quote, .paragraph])
        XCTAssertEqual(Blocks.parse(notes["quote"]!).first?.text, "quoted\n\nsecond paragraph")
        XCTAssertEqual(Blocks.parse(notes["rules"]!).map(\.kind), [.rule, .rule, .rule, .rule])
        XCTAssertEqual(Blocks.parse("-not a list\n1.not numbered\n").map(\.kind), [.paragraph])
        XCTAssertEqual(Blocks.parse("- [ ]\n").first?.kind, .task(checked: false))
        XCTAssertEqual(Blocks.parse("- [ ]x\n").first?.kind, .bullet)
    }

    /// Touching a block that is already in the canonical form the editor
    /// writes must not change a byte. Blank blocks are left untouched, as
    /// every save path drops them before writing.
    func testCanonicalBlocksRoundTripWhenTouched() {
        let canonical = "# Title\n\nA paragraph with **bold**, _italic_, `code`, ~~gone~~ and [[a link]].\nIt wraps.\n\n- bullet\n  - nested\n- [ ] task\n- [x] done\n\n1. first\n2) second\n\n> quote\n>\n> more\n\n___\n"
        var document = EditorDocument(markdown: canonical)
        for index in document.blocks.indices where document.blocks[index].kind != .blank {
            document.blocks[index].touched = true
        }
        XCTAssertEqual(document.markdown(), canonical)
    }

    func testNewBlocksAreSeparatedOnlyWhereMarkdownNeedsIt() {
        var document = EditorDocument(markdown: "Existing\n")
        document.blocks.append(Block(id: 1, kind: .paragraph, text: "New paragraph"))
        document.blocks.append(Block(id: 2, kind: .bullet, text: "one"))
        document.blocks.append(Block(id: 3, kind: .task(checked: false), text: "two"))
        document.blocks.append(Block(id: 4, kind: .numbered(marker: "1."), text: "three"))
        XCTAssertEqual(document.lines(), ["Existing", "", "New paragraph", "", "- one", "- [ ] two", "", "1. three"])
    }

    func testEditingNextToAnOpaqueBlockLeavesItAlone() throws {
        let text = "Intro\n\n| a | b |\n|---|---|\n\nOutro\n"
        var document = EditorDocument(markdown: text)
        let index = try XCTUnwrap(document.blocks.firstIndex { $0.text == "Outro" })
        document.blocks[index].text = "Changed"
        document.blocks[index].touched = true
        XCTAssertEqual(document.markdown(), "Intro\n\n| a | b |\n|---|---|\n\nChanged\n")
    }

    // MARK: Inline

    func testCanonicalInlineMarkupRoundTrips() {
        for text in ["", "plain", "**bold** and _italic_", "**bold _both_ bold**", "**~~struck bold~~**",
                     "`a * b`", "[[note]]", "[[folder/note|label]]", "[label](https://a.test/x_y?q=1)",
                     "**[[bold link]]**", "2 * 3 * 4", "a_b_c", "snake_case and init_.py",
                     "![image](a.png) stays literal", "<span>html</span> literal", "trailing *", "** not bold **", "emoji 👩‍👩‍👧 **bold 🎉**"] {
            XCTAssertEqual(Inline.serialize(Inline.parse(text)), text, text)
        }
    }

    /// Nesting is written bold, then italic, then strike, and `__` as `**`:
    /// the Markdown changes but it reads back as the same styles.
    func testOtherSpellingsNormaliseToTheSameStyles() {
        for (text, normalised) in [("_italic **both**_", "_italic_ **_both_**"), ("~~**x**~~", "**~~x~~**"), ("__init__.py", "**init**.py"), ("*a*", "_a_")] {
            XCTAssertEqual(Inline.serialize(Inline.parse(text)), normalised, text)
            XCTAssertEqual(Inline.plainText(normalised), Inline.plainText(text), text)
        }
    }

    func testInlineStyles() {
        XCTAssertEqual(Inline.parse("__bold__"), [InlineRun("bold", bold: true)])
        XCTAssertEqual(Inline.parse("*italic*"), [InlineRun("italic", italic: true)])
        XCTAssertEqual(Inline.parse("a ~~b~~"), [InlineRun("a "), InlineRun("b", strike: true)])
        XCTAssertEqual(Inline.parse("`**x**`"), [InlineRun("**x**", code: true)])
        XCTAssertEqual(Inline.parse("[x](https://a.test)"), [InlineRun("x", link: .web("https://a.test"))])
        XCTAssertEqual(Inline.parse("**a *b* c**"), [InlineRun("a ", bold: true), InlineRun("b", bold: true, italic: true), InlineRun(" c", bold: true)])
        XCTAssertEqual(Inline.parse("\\*not italic\\*"), [InlineRun("*not italic*")])
        XCTAssertEqual(Inline.parse("**unclosed"), [InlineRun("**unclosed")])
        XCTAssertEqual(Inline.plainText("**Ana** [[notes/plan|the plan]] `x`"), "Ana the plan x")
    }

    func testSerializedRunsReadBackAsTheSameRuns() {
        let runs = [InlineRun("Coffee "), InlineRun("with", bold: true), InlineRun(" "), InlineRun("Ana", italic: true), InlineRun(" at "), InlineRun("9", code: true),
                    InlineRun(" see "), InlineRun("plan", link: .note("notes/plan")), InlineRun(" or "), InlineRun("site", link: .web("https://a.test"))]
        XCTAssertEqual(Inline.parse(Inline.serialize(runs)), Inline.merged(runs))
    }

    /// The editor saves what it shows as runs, so runs must carry the user's
    /// exact characters back to Markdown.
    func testLiteralMarkupCharactersSurviveTheRuns() {
        for text in ["\\*not italic\\*", "\\_not\\_ italic", "a \\`tick\\`", "\\[[not a link]]", "\\\\", "\\<!-- shown -->", "\\~~not struck~~"] {
            let back = Inline.serialize(Inline.parse(text))
            XCTAssertEqual(Inline.parse(back), Inline.parse(text), text)
        }
        let typed: [[InlineRun]] = [
            [InlineRun("*not italic* and [[not a link]] and <!-- not hidden -->")],
            [InlineRun("\\")],
            [InlineRun("a\\*b*")],
            [InlineRun("*a\\*")],
            [InlineRun("path\\"), InlineRun("x", bold: true)],
            [InlineRun("a*", bold: true)],
            [InlineRun("*"), InlineRun("x", italic: true), InlineRun("*")],
            [InlineRun("`tick`"), InlineRun("code", code: true)],
            [InlineRun("~~"), InlineRun("x", strike: true)],
        ]
        for runs in typed {
            XCTAssertEqual(Inline.parse(Inline.serialize(runs)), Inline.merged(runs), "\(runs)")
        }
    }

    /// Escaping only where markup would be read keeps lone markup characters
    /// as the person typed them.
    func testTextThatNeedsNoEscapingIsNotEscaped() {
        for text in ["snake_case_word", "2 * 3 * 4", "a_b", "[not a link]", "a ~ b", "it`s", "a < b <!-- unclosed", "\\ alone", "C:\\path"] {
            XCTAssertEqual(Inline.serialize(Inline.parse(text)), text, text)
        }
        XCTAssertEqual(Inline.serialize([InlineRun("*not italic*")]), "\\*not italic*")
        XCTAssertEqual(Inline.serialize([InlineRun("\\")]), "\\")
    }

    func testAnInlineCommentSurvivesTheRuns() {
        for text in ["Standup <!--gcal:9f2c-->", "Call <!--inbox:ab12--> Ana", "<!--x--> first", "[[Ana]] <!--inbox:ab12-->",
                     "[site](https://a.test)<!--id--> after", "**bold** <!--id-->", "two  <!--a--><!--b-->"] {
            XCTAssertEqual(Inline.serialize(Inline.parse(text)), text, text)
        }
        XCTAssertEqual(Inline.parse("Standup <!--gcal:9f2c-->"), [InlineRun("Standup"), InlineRun(" <!--gcal:9f2c-->", comment: true)])
        XCTAssertEqual(Inline.plainText("Call <!--inbox:ab12--> Ana"), "Call Ana")
        XCTAssertFalse(NoteView.isItalicOnly("<!--x-->"))
        XCTAssertTrue(NoteView.isItalicOnly("_a hint_ <!--x-->"))
    }

    func testACodeSpanHoldingABacktickSurvivesTheRuns() {
        let text = "``a ` b``"
        XCTAssertEqual(Inline.parse(Inline.serialize(Inline.parse(text))), Inline.parse(text))
        XCTAssertEqual(Inline.serialize(Inline.parse(text)), text)
        for code in ["`", "`x", "x`", "a``b", "a\\", "a\\`b"] {
            let runs = [InlineRun("see "), InlineRun(code, code: true)]
            XCTAssertEqual(Inline.parse(Inline.serialize(runs)), runs, code)
        }
        XCTAssertEqual(Inline.serialize([InlineRun("`x", code: true)]), "`` `x ``")
    }

    func testALinkWithStyledLabelStaysOneLink() {
        for text in ["[**bold** label](https://a.test)", "[[notes/plan|the **big** plan]]", "**[[bold link]]**", "_[a **b**](https://a.test)_",
                     "[[note]] and [[other]]"] {
            XCTAssertEqual(Inline.serialize(Inline.parse(text)), text, text)
        }
        let runs = [InlineRun("the "), InlineRun("big", bold: true, link: .note("plan")), InlineRun(" plan", link: .note("plan"))]
        XCTAssertEqual(Inline.serialize(runs), "the [[plan|**big** plan]]")
        XCTAssertEqual(Inline.parse(Inline.serialize(runs)), runs)
        XCTAssertEqual(Inline.serialize([InlineRun("plan", italic: true, link: .note("plan"))]), "_[[plan]]_")
    }
}
