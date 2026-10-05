import XCTest
@testable import ThockKit

final class VaultTests: XCTestCase {
    let day = VaultDay(year: 2026, month: 10, day: 2)
    var config: VaultConfig { VaultConfig(config: SampleVault.config) }

    func repoFile(_ relative: String) -> String? {
        var directory = URL(fileURLWithPath: #filePath)
        for _ in 0..<8 {
            directory.deleteLastPathComponent()
            let candidate = directory.appendingPathComponent(relative)
            if let text = try? String(contentsOf: candidate, encoding: .utf8) {
                return text
            }
        }
        return nil
    }

    func writes(now: Date = Date(timeIntervalSince1970: 1_790_975_640)) -> PhoneWrites {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return PhoneWrites(config: config, deviceID: "c41a9e0d7b2f4a61", now: now, calendar: calendar)
    }

    func testDatesFormatLikeTheDesk() {
        XCTAssertEqual(day.formatted("dddd, MMMM D, YYYY"), "Friday, October 2, 2026")
        XCTAssertEqual(day.formatted("YYYY-MM-DD"), "2026-10-02")
        XCTAssertEqual(day.formatted("GGGG-[W]WW"), "2026-W40")
        XCTAssertEqual(VaultDay(year: 2027, month: 1, day: 1).formatted("GGGG-[W]WW"), "2026-W53")
        XCTAssertEqual(day.formatted("YY M D ddd dd d MMM W"), "26 10 2 Fri Fr 5 Oct 40")
        XCTAssertEqual(config.dailyPath(day), "daily/2026-10-02.md")
        XCTAssertEqual(config.weeklyPath(day), "weekly/2026-W40.md")
        XCTAssertEqual(day.adding(days: -2).iso, "2026-09-30")
        XCTAssertEqual(Template.expand("# {{date:dddd, MMMM D, YYYY}} {{time}} {{title}} {{nope}} {{date}}", day: day, time: "09:41", title: "t"),
                       "# Friday, October 2, 2026 09:41 t {{nope}} 2026-10-02")
    }

    func testConfigReadsWhatThePhoneNeeds() {
        let config = VaultConfig(config: """
            schema = 1
            [daily]
            dir = "journal/days" # a comment
            [backlog]
            file = "tasks.md"
            headings = { soon = "Em breve", someday = "Algum dia" }
            [day_planner]
            heading = ["Agenda", "Day planner"]
            [[routines.installed]]
            id = "timeline"
            """, inboxConfig: "dir = \"caixa/\"\n")
        XCTAssertEqual(config.daily.dir, "journal/days")
        XCTAssertEqual(config.daily.filename, "YYYY-MM-DD")
        XCTAssertEqual(config.backlogFile, "tasks.md")
        XCTAssertEqual(config.soonHeading, "Em breve")
        XCTAssertEqual(config.somedayHeading, "Algum dia")
        XCTAssertEqual(config.completedHeading, "Completed")
        XCTAssertEqual(config.plannerHeadings, ["Agenda", "Day planner"])
        XCTAssertEqual(config.inboxDir, "caixa")
        XCTAssertEqual(VaultConfig(config: "not = [toml").plannerHeadings, ["Day planner"])
    }

    func testTodayIsDrawnFromTheNotesOwnSections() throws {
        let seed = SampleVault.make(today: day)
        let view = NoteView(text: try XCTUnwrap(seed.files["daily/2026-10-02.md"]), config: config)
        XCTAssertEqual(view.title, "Friday, October 2, 2026")
        XCTAssertEqual(view.cards.map(\.title), ["Journal", "Day planner", "Personal"])
        XCTAssertEqual(view.cards.map(\.kind), [.journal, .planner, .prose])
        XCTAssertEqual(view.planner.groups.map(\.name), [nil, "Calendar"])
        XCTAssertEqual(view.planner.groups[0].items.map(\.label), ["Morning walk 🚶", "Deep work: the budget spreadsheet 📊", "Call the dentist ☎️", "Buy a birthday card for Dad 🎂", "Read 20 pages 📖"])
        XCTAssertEqual(view.planner.groups[1].items.map(\.label), ["Standup", "Lunch with Ana 🥗"])
        XCTAssertTrue(view.planner.groups[1].items.allSatisfy(\.isCalendar))
        XCTAssertFalse(view.planner.groups[0].items.contains(where: \.isCalendar))
        XCTAssertEqual(view.planner.doneCount, 2)
        XCTAssertEqual(view.planner.items.count, 7)
        XCTAssertEqual(view.planner.items[1].time?.raw, "09:30 - 11:00")
        XCTAssertEqual(view.journal.entries.map(\.time), [nil, nil, "13:02"])
        XCTAssertEqual(view.journal.entries.map(\.isPrompt), [true, false, false])
        XCTAssertEqual(view.journal.written.last?.text, "Noticed I keep saying yes to things on Thursdays. Worth watching.")

        let yesterday = NoteView(text: try XCTUnwrap(seed.files["daily/2026-10-01.md"]), config: config)
        XCTAssertEqual(yesterday.cards.map(\.kind), [.journal, .planner, .prose, .agent])
        XCTAssertEqual(yesterday.cards.last?.title, "Daily Closure")
        XCTAssertTrue(yesterday.cards[2].blocks.contains { $0.kind == .opaque && $0.source.count == 4 })
        XCTAssertTrue(try XCTUnwrap(yesterday.planner.items.last).struck)
    }

    func testAHeadingRenamedOrDecoratedStillFindsThePlanner() {
        var config = config
        config.plannerHeadings = ["Agenda", "Day planner"]
        let view = NoteView(text: "# Day\n\n## 📅 **Day planner**:\n- [ ] A\n", config: config)
        XCTAssertTrue(view.planner.exists)
        XCTAssertEqual(view.planner.heading.text, "📅 **Day planner**:")
        XCTAssertEqual(NoteView(text: "# Day\n\n## Notes\n", config: config).planner.heading.text, "Agenda")
    }

    func testATickLandsOnTheTaskNotABulletWithTheSameWords() throws {
        let note = "# Day\n\n## Day planner\n\n- Call Ana\n- [ ] Call Ana\n"
        let view = NoteView(text: note, config: config)
        let task = try XCTUnwrap(view.planner.items.first)
        XCTAssertEqual(task.ordinal, 1)
        let tick = try XCTUnwrap(writes().tick(task, planner: view.planner, note: .day(day)))
        XCTAssertEqual(SyncCore.apply(existing: note, write: tick.document).text, "# Day\n\n## Day planner\n\n- Call Ana\n- [x] Call Ana\n")
    }

    func testTheRulesReadMarkdownAsTheDeskDoes() throws {
        XCTAssertNil(TextFile.heading(in: "## ###"))
        XCTAssertEqual(TextFile.heading(in: "## C#")?.text, "C#")
        XCTAssertEqual(TextFile.heading(in: "## Plan ##")?.text, "Plan")
        XCTAssertTrue(TextFile.isThematicBreak("_ _ _"))
        XCTAssertTrue(TextFile.isThematicBreak("   ___"))
        XCTAssertFalse(TextFile.isThematicBreak("    ___"))
        XCTAssertFalse(TextFile.isThematicBreak("\t___"))
        XCTAssertEqual(SyncCore.headingKey("[Plan](my file (v2).md)"), SyncCore.headingKey("Plan"))
        // A closing delimiter with a trailing space does not close front matter.
        XCTAssertEqual(TextFile("--- \ntitle: x\n---\n# Day\n").contentMask(), [true, true, true, true])

        var stale = WriteDocument(clientID: "c", kind: .replaceSection, path: "a.md", madeAt: "", deviceID: "d")
        stale.heading = HeadingRef(text: "A")
        stale.baseHash = "0"
        stale.lines = ["", "  "]
        XCTAssertEqual(SyncCore.apply(existing: "## A\nx\n", write: stale).outcome, .noop)

        var created = WriteDocument(clientID: "c", kind: .append, path: "a.md", madeAt: "", deviceID: "d")
        created.heading = HeadingRef(text: " Journal ", level: 0)
        created.lines = ["y"]
        XCTAssertEqual(SyncCore.apply(existing: "x\n", write: created).text, "x\n\n# Journal\ny\n")

        let base = #""client_id":"c","path":"a.md","kind":"append","heading":null"#
        XCTAssertNoThrow(try WriteDocument.parse(#"{"v":1,"# + base + #","lines":[]}"#))
        XCTAssertThrowsError(try WriteDocument.parse("{" + base + #","lines":[]}"#))
        XCTAssertThrowsError(try WriteDocument.parse(#"{"v":1,"# + base + "}"))
        XCTAssertThrowsError(try WriteDocument.parse(#"{"v":1,"client_id":"c","path":"a.md","kind":"remove_line","heading":null,"line_hash":"h","ordinal":-1}"#))
        XCTAssertThrowsError(try WriteDocument.parse(#"{"v":1,"client_id":"c","path":"a.md","kind":"append","heading":{"text":"  "},"lines":[]}"#))
        XCTAssertFalse(SyncCore.isSyncablePath("daily\\x.md"))
    }

    func testAFolderWithASlashStillNamesTheNote() {
        let config = VaultConfig(config: "[daily]\ndir = \"./notes/daily/\"\n[weekly]\ndir = \".\"\n")
        XCTAssertEqual(config.dailyPath(day), "notes/daily/2026-10-02.md")
        XCTAssertEqual(config.weeklyPath(day), "2026-W40.md")
    }

    func testThePlannersCalendarIsNeverACardOfItsOwn() {
        // A planner at level 1 puts the desk's calendar at level 2, below the
        // title's section; it is drawn once, inside the planner.
        let note = """
            # Friday

            ## Journal

            Words.

            # Day planner

            - [ ] A

            ## Calendar

            - [ ] 10:00 - 10:30 Standup <!--gcal:9f2c1ab4e7d0-->

            # Daily Closure

            ## Highlights

            Done.
            """
        let view = NoteView(text: note, config: config)
        XCTAssertEqual(view.cards.map(\.title), ["Journal", "Day planner", "Daily Closure"])
        XCTAssertEqual(view.cards.map(\.kind), [.journal, .planner, .agent])
        XCTAssertEqual(view.planner.groups.map(\.name), [nil, "Calendar"])
        XCTAssertTrue(view.cards[2].blocks.contains { $0.kind == .heading(level: 2) && $0.text.contains("Highlights") })
    }

    // MARK: V33 §16, round-trip

    func corpus() -> [String: String] {
        var notes: [String: String] = [
            "daily template": Template.defaultDaily,
            "weekly template": Template.defaultWeekly,
            "frontmatter with comments": "---\ntitle: x # not a heading\ntags: [a, b]\n---\n\n# Title\n\nBody.\n",
            "code with a fake heading": "Text\n\n```\n## not a heading\n- [ ] not a task\n```\n\nMore\n",
            "table": "| a | b |\n| --- | --- |\n| 1 | 2 |\n\nAfter\n",
            "comment": "<!-- a\nmultiline comment -->\n\nText <!-- inline -->\n",
            "nested lists": "- one\n  - two\n    - [ ] three\n\t* tab\n1. first\n2) second\n",
            "quotes and rules": "> a\n> b\n\n---\n\n***\n___\n",
            "hard wrapped": "A paragraph that\nwraps over lines\nthree times.\n\nAnother.\n",
            "duplicates": "## Day planner\n- [ ] Buy milk\n- [ ] Buy milk\n",
            "agent headings": "# Day\n\n## Journal\n\nText\n\n# Daily Closure\n\nDone.\n",
            "raw html": "<div align=\"center\">\n<b>hi</b>\n</div>\n\nText\n",
            "odd spacing": "\n\n#  Spaced   heading  \n\n\n-   wide bullet\n- [x]   wide task\n",
        ]
        for (path, text) in SampleVault.make(today: day).files {
            notes["sample " + path] = text
        }
        if let example = repoFile("crates/thock/assets/example-day.md") {
            notes["example day"] = example
        }
        return notes
    }

    func testANoteOpenedAndSavedWithoutEditsIsByteIdentical() {
        for (name, text) in corpus() {
            let lines = TextFile(text).lines.map(\.text)
            XCTAssertEqual(EditorDocument(markdown: text).lines(), lines, name)
            XCTAssertEqual(TextFile(text).text, text, name)
        }
        let crlf = "## A\r\n\r\nText\r\n"
        XCTAssertEqual(TextFile(crlf).text, crlf)
    }

    func testEditingOneParagraphChangesOnlyItsLines() throws {
        for (name, text) in corpus() {
            var document = EditorDocument(markdown: text)
            guard let index = document.blocks.firstIndex(where: { $0.kind == .paragraph }) else { continue }
            let before = document.lines()
            document.blocks[index].text = "Edited **on** the phone."
            document.blocks[index].touched = true
            let after = document.lines()
            let block = document.blocks[index]
            let start = block.line
            XCTAssertEqual(Array(after[..<start]), Array(before[..<start]), name)
            XCTAssertEqual(after[start], "Edited **on** the phone.", name)
            XCTAssertEqual(Array(after[(start + 1)...]), Array(before[(start + block.source.count)...]), name)
        }
    }

    func testInlineMarkupSurvivesTheEditor() {
        for text in ["Plain", "Coffee with **Ana**, who _is_ thinking", "See [[welcome]] and [[notes/plan|the plan]]", "A [link](https://example.com/a_b) here",
                     "Mixed **bold _both_** end", "`code` stays", "~~gone~~ kept", "snake_case_name and 2 * 3 * 4"] {
            XCTAssertEqual(Inline.serialize(Inline.parse(text)), text, text)
        }
        XCTAssertEqual(Inline.parse("**Ana** left").first, InlineRun("Ana", bold: true))
        XCTAssertEqual(Inline.parse("[[notes/plan|the plan]]").first, InlineRun("the plan", link: .note("notes/plan")))
        XCTAssertEqual(Inline.plainText("Standup <!--gcal:9f2c-->"), "Standup")
        XCTAssertEqual(Inline.serialize([InlineRun("bold ", bold: true), InlineRun("after")]), "**bold** after")
    }

    // MARK: V33 §16, single-line moves

    let planner = """
    # Day

    ## Day planner

    - [x] 08:00 - 08:30 Morning walk
    - [ ] 09:30 - 11:00 Deep work
    - [ ] Buy a card

    ### Calendar

    - [ ] 12:30 Lunch with Ana <!--gcal:5b7e3c9a1f24-->

    ## Personal

    Text

    """

    func changedLines(_ before: String, _ after: String) -> (removed: [String], added: [String]) {
        let old = TextFile(before).lines.map(\.text)
        let new = TextFile(after).lines.map(\.text)
        let difference = new.difference(from: old)
        var removed: [String] = []
        var added: [String] = []
        for change in difference {
            switch change {
            case .remove(_, let line, _): removed.append(line)
            case .insert(_, let line, _): added.append(line)
            }
        }
        return (removed, added)
    }

    func testEveryNudgeChangesExactlyOneLine() throws {
        let builder = writes()
        let view = NoteView(text: planner, config: config)
        let deepWork = try XCTUnwrap(view.planner.items.first { $0.label == "Deep work" })
        let card = try XCTUnwrap(view.planner.items.first { $0.label == "Buy a card" })

        func apply(_ planned: PlannedWrite?) throws -> String {
            SyncCore.apply(existing: planner, write: try XCTUnwrap(planned).document).text
        }

        var change = changedLines(planner, try apply(builder.tick(deepWork, planner: view.planner, note: .day(day))))
        XCTAssertEqual(change.removed, ["- [ ] 09:30 - 11:00 Deep work"])
        XCTAssertEqual(change.added, ["- [x] 09:30 - 11:00 Deep work"])

        change = changedLines(planner, try apply(builder.setTime(card, startMinutes: 15 * 60, endMinutes: 15 * 60 + 30, planner: view.planner, note: .day(day))))
        XCTAssertEqual(change.removed, ["- [ ] Buy a card"])
        XCTAssertEqual(change.added, ["- [ ] 15:00 - 15:30 Buy a card"])

        change = changedLines(planner, try apply(builder.setTime(deepWork, startMinutes: nil, endMinutes: nil, planner: view.planner, note: .day(day))))
        XCTAssertEqual(change.added, ["- [ ] Deep work"])

        change = changedLines(planner, try apply(builder.editText(deepWork, text: "Deep work: the budget", planner: view.planner, note: .day(day))))
        XCTAssertEqual(change.removed, ["- [ ] 09:30 - 11:00 Deep work"])
        XCTAssertEqual(change.added, ["- [ ] 09:30 - 11:00 Deep work: the budget"])

        change = changedLines(planner, try apply(builder.remove(card, planner: view.planner, note: .day(day))))
        XCTAssertEqual(change.removed, ["- [ ] Buy a card"])
        XCTAssertEqual(change.added, [])

        change = changedLines(planner, try apply(builder.addLine("Water the plants", group: nil, planner: view.planner, note: .day(day))))
        XCTAssertEqual(change.removed, [])
        XCTAssertEqual(change.added, ["- [ ] Water the plants"])
        XCTAssertTrue(try apply(builder.addLine("Water the plants", group: nil, planner: view.planner, note: .day(day))).contains("- [ ] Buy a card\n- [ ] Water the plants\n\n### Calendar"))

        let calendar = view.planner.groups[1]
        XCTAssertTrue(try apply(builder.addLine("Dentist", group: calendar, planner: view.planner, note: .day(day))).contains("<!--gcal:5b7e3c9a1f24-->\n- [ ] Dentist\n\n## Personal"))

        let moved = builder.moveToSoon(deepWork, planner: view.planner, note: .day(day))
        XCTAssertEqual(moved.map(\.document.path), ["daily/2026-10-02.md", "backlog.md"])
        change = changedLines(planner, SyncCore.apply(existing: planner, write: moved[0].document).text)
        XCTAssertEqual(change.removed, ["- [ ] 09:30 - 11:00 Deep work"])
        XCTAssertEqual(change.added, [])
        let backlog = "# Backlog\n\n## Soon\n\n- [ ] Renew the passport\n\n### Home\n\n- [ ] Fix the light\n\n## Someday\n"
        XCTAssertEqual(SyncCore.apply(existing: backlog, write: moved[1].document).text,
                       "# Backlog\n\n## Soon\n\n- [ ] Renew the passport\n- [ ] Deep work\n\n### Home\n\n- [ ] Fix the light\n\n## Someday\n")
    }

    // MARK: V37 §10, the week

    func testWeeksAreNamedAsTheVaultNamesThem() {
        // 2026 has 53 ISO weeks; its last one runs into January 2027.
        let lastMonday = VaultDay(year: 2026, month: 12, day: 28)
        XCTAssertEqual(VaultWeek(lastMonday), VaultWeek(year: 2026, week: 53))
        XCTAssertEqual(VaultWeek(VaultDay(year: 2027, month: 1, day: 1)), VaultWeek(year: 2026, week: 53))
        XCTAssertEqual(VaultWeek(VaultDay(year: 2027, month: 1, day: 4)), VaultWeek(year: 2027, week: 1))
        XCTAssertEqual(VaultWeek(year: 2026, week: 53).monday, lastMonday)
        XCTAssertEqual(VaultWeek(year: 2027, week: 1).monday, VaultDay(year: 2027, month: 1, day: 4))
        XCTAssertEqual(VaultWeek(year: 2026, week: 1).monday, VaultDay(year: 2025, month: 12, day: 29))
        XCTAssertEqual(config.weeklyPath(VaultWeek(year: 2026, week: 53)), "weekly/2026-W53.md")
        XCTAssertEqual(config.path(.week(VaultWeek(day))), config.weeklyPath(day))

        let week = VaultWeek(day)
        XCTAssertEqual(week.days.map(\.iso), ["2026-09-28", "2026-09-29", "2026-09-30", "2026-10-01", "2026-10-02", "2026-10-03", "2026-10-04"])
        XCTAssertTrue(week.contains(day))
        XCTAssertEqual(week.adding(weeks: 1), VaultWeek(year: 2026, week: 41))
        XCTAssertEqual(week.weeks(until: VaultWeek(year: 2027, week: 1)), 14)
        // Every day of every week agrees with the day's own week fields.
        var probe = VaultDay(year: 2024, month: 12, day: 20)
        for _ in 0..<800 {
            XCTAssertTrue(VaultWeek(probe).contains(probe), probe.iso)
            XCTAssertEqual(config.weeklyPath(VaultWeek(probe)), config.weeklyPath(probe), probe.iso)
            probe = probe.adding(days: 1)
        }
    }

    func testAMonthIsRowsOfWholeWeeks() {
        let october = VaultMonth(year: 2026, month: 10)
        XCTAssertEqual(october.weeks.map(\.week), [40, 41, 42, 43, 44])
        XCTAssertEqual(october.weeks.first?.days.first?.iso, "2026-09-28")
        XCTAssertEqual(october.weeks.last?.days.last?.iso, "2026-11-01")
        XCTAssertEqual(october.name, "October 2026")
        XCTAssertEqual(october.adding(months: 3), VaultMonth(year: 2027, month: 1))
        XCTAssertEqual(october.adding(months: -10), VaultMonth(year: 2025, month: 12))
        XCTAssertEqual(VaultMonth(year: 2027, month: 1).weeks.map { "\($0.year)-W\($0.week)" }, ["2026-W53", "2027-W1", "2027-W2", "2027-W3", "2027-W4"])
        XCTAssertEqual(VaultMonth(year: 2026, month: 2).weeks.count, 5)
        XCTAssertEqual(VaultMonth(year: 2027, month: 2).weeks.count, 4)
        XCTAssertNil(config.day(ofDailyPath: "weekly/2026-W40.md"))
        XCTAssertEqual(config.day(ofDailyPath: "daily/2026-10-02.md"), day)
        XCTAssertNil(config.day(ofDailyPath: "daily/old/2026-10-02.md"))
    }

    let weekly = """
    # Week 40, 2026

    _Seven days, one page._

    ___

    ## Goals

    _Two or three things that would make this a good week._

    - [x] Ship the sync status dot
    - [ ] Read 100 pages

    ___

    ## Notes

    A lighter week on purpose.
    Thursday is the only full day.

    ## Week review

    _How did it go?_

    # AI Week Review

    Four of five planned days had deep work before noon.

    ## Things Thock could forget

    - [ ] The Sunday idea

    """

    func testTheWeekIsDrawnFromItsNoteWithGoalsAsItsChecklist() throws {
        let view = NoteView(text: weekly, config: config, kind: .weekly)
        XCTAssertEqual(view.cards.map(\.kind), [.preamble, .planner, .prose, .prose, .agent])
        XCTAssertEqual(view.cards.map(\.title), ["", "Goals", "Notes", "Week review", "AI Week Review"])
        XCTAssertEqual(view.planner.heading.text, "Goals")
        XCTAssertEqual(view.planner.items.map(\.label), ["Ship the sync status dot", "Read 100 pages"])
        XCTAssertEqual(view.planner.doneCount, 1)
        XCTAssertFalse(view.journal.exists)
        // Only the user's prose paragraphs are editable: not the preamble,
        // not the agent's text, not its checklist.
        let notes = try XCTUnwrap(view.cards.first { $0.title == "Notes" })
        let paragraph = try XCTUnwrap(notes.blocks.first { $0.kind == .paragraph })
        XCTAssertTrue(view.isEditable(paragraph, in: notes))
        let preamble = view.cards[0]
        XCTAssertFalse(view.isEditable(try XCTUnwrap(preamble.blocks.first { $0.kind == .paragraph }), in: preamble))
        let agent = view.cards[4]
        XCTAssertFalse(view.isEditable(try XCTUnwrap(agent.blocks.first { $0.kind == .paragraph }), in: agent))
        // The template's prompt is drawn, not replaced: a tap on it starts a paragraph.
        let review = try XCTUnwrap(view.cards.first { $0.title == "Week review" })
        let prompt = try XCTUnwrap(review.blocks.first { $0.kind == .paragraph })
        XCTAssertTrue(NoteView.isPrompt(prompt))
        XCTAssertFalse(view.isEditable(prompt, in: review))
        // A daily note is unchanged by the weekly rules: its goals, if any, are prose.
        XCTAssertEqual(NoteView(text: weekly, config: config).planner.exists, false)
    }

    func testEveryGoalMoveChangesExactlyOneLine() throws {
        let builder = writes()
        let week = NoteID.week(VaultWeek(day))
        let view = NoteView(text: weekly, config: config, kind: .weekly)
        let pages = try XCTUnwrap(view.planner.items.first { $0.label == "Read 100 pages" })

        func apply(_ planned: PlannedWrite?) throws -> String {
            SyncCore.apply(existing: weekly, write: try XCTUnwrap(planned).document).text
        }

        var change = changedLines(weekly, try apply(builder.tick(pages, planner: view.planner, note: week)))
        XCTAssertEqual(change.removed, ["- [ ] Read 100 pages"])
        XCTAssertEqual(change.added, ["- [x] Read 100 pages"])

        change = changedLines(weekly, try apply(builder.editText(pages, text: "Read 60 pages", planner: view.planner, note: week)))
        XCTAssertEqual(change.removed, ["- [ ] Read 100 pages"])
        XCTAssertEqual(change.added, ["- [ ] Read 60 pages"])

        change = changedLines(weekly, try apply(builder.remove(pages, planner: view.planner, note: week)))
        XCTAssertEqual(change.removed, ["- [ ] Read 100 pages"])
        XCTAssertEqual(change.added, [])

        let added = try apply(builder.addLine("Two no-screen evenings", group: nil, planner: view.planner, note: week))
        XCTAssertEqual(changedLines(weekly, added).added, ["- [ ] Two no-screen evenings"])
        XCTAssertTrue(added.contains("- [ ] Read 100 pages\n- [ ] Two no-screen evenings\n\n___\n\n## Notes"))
        let addLine = try XCTUnwrap(builder.addLine("x", group: nil, planner: view.planner, note: week))
        XCTAssertEqual(addLine.document.path, "weekly/2026-W40.md")
        XCTAssertEqual(addLine.seed, SeedInfo(kind: .weekly, day: VaultDay(year: 2026, month: 9, day: 28), time: builder.clock))

        let moved = builder.moveToSoon(pages, planner: view.planner, note: week)
        XCTAssertEqual(moved.map(\.document.path), ["weekly/2026-W40.md", "backlog.md"])
        XCTAssertEqual(changedLines(weekly, SyncCore.apply(existing: weekly, write: moved[0].document).text).removed, ["- [ ] Read 100 pages"])
        XCTAssertEqual(SyncCore.apply(existing: "# Backlog\n\n## Soon\n\n- [ ] Renew the passport\n\n## Someday\n", write: moved[1].document).text,
                       "# Backlog\n\n## Soon\n\n- [ ] Renew the passport\n- [ ] Read 100 pages\n\n## Someday\n")

        // The agent's checklist is not the week's goals.
        XCTAssertFalse(view.planner.items.contains { $0.label == "The Sunday idea" })
    }

    func testAParagraphIsReplacedOrAddedInPlace() throws {
        let builder = writes()
        let week = NoteID.week(VaultWeek(day))
        let view = NoteView(text: weekly, config: config, kind: .weekly)
        let notes = try XCTUnwrap(view.cards.first { $0.title == "Notes" })
        let wrapped = try XCTUnwrap(notes.blocks.first { $0.kind == .paragraph })
        XCTAssertEqual(wrapped.source.count, 2)

        // A wrapped paragraph rewrites its section, guarded by the section's hash.
        let replaced = try XCTUnwrap(builder.replaceParagraph(wrapped, with: "A lighter week on purpose.", heading: try XCTUnwrap(notes.heading), note: week, text: weekly))
        XCTAssertEqual(replaced.document.kind, .replaceSection)
        var change = changedLines(weekly, SyncCore.apply(existing: weekly, write: replaced.document).text)
        XCTAssertEqual(change.removed, ["Thursday is the only full day."])
        XCTAssertEqual(change.added, [])

        // A one-line paragraph is a single-line replacement.
        let review = try XCTUnwrap(view.cards.first { $0.title == "Week review" })
        let prompt = try XCTUnwrap(review.blocks.first { $0.kind == .paragraph })
        let fixed = try XCTUnwrap(builder.replaceParagraph(prompt, with: "Good week for shipping.", heading: try XCTUnwrap(review.heading), note: week, text: weekly))
        XCTAssertEqual(fixed.document.kind, .replaceLine)
        change = changedLines(weekly, SyncCore.apply(existing: weekly, write: fixed.document).text)
        XCTAssertEqual(change.removed, ["_How did it go?_"])
        XCTAssertEqual(change.added, ["Good week for shipping."])

        // An added paragraph lands at the end of its section, before the next heading.
        let appended = try XCTUnwrap(builder.appendParagraph(blocks: Blocks.parse("Keep Friday afternoon free."), heading: try XCTUnwrap(review.heading), note: week))
        XCTAssertEqual(appended.seed?.kind, .weekly)
        let after = SyncCore.apply(existing: weekly, write: appended.document).text
        XCTAssertEqual(changedLines(weekly, after).added.filter { !$0.isEmpty }, ["Keep Friday afternoon free."])
        XCTAssertEqual(changedLines(weekly, after).removed, [])
        XCTAssertTrue(after.contains("_How did it go?_\n\nKeep Friday afternoon free.\n\n# AI Week Review"))
        XCTAssertNil(builder.appendParagraph(blocks: Blocks.parse("   "), heading: try XCTUnwrap(review.heading), note: week))

        // The same on a day's prose section.
        let dayView = NoteView(text: planner, config: config)
        let personal = try XCTUnwrap(dayView.cards.first { $0.title == "Personal" })
        let text = try XCTUnwrap(personal.blocks.first { $0.kind == .paragraph })
        XCTAssertTrue(dayView.isEditable(text, in: personal))
        let edited = try XCTUnwrap(builder.replaceParagraph(text, with: "Text, edited.", heading: try XCTUnwrap(personal.heading), note: .day(day), text: planner))
        XCTAssertEqual(edited.document.path, "daily/2026-10-02.md")
        change = changedLines(planner, SyncCore.apply(existing: planner, write: edited.document).text)
        XCTAssertEqual(change.removed, ["Text"])
        XCTAssertEqual(change.added, ["Text, edited."])
    }

    func testATickKeepsEverythingElseOnTheLine() throws {
        let line = "\t* [ ] 10:00 - 10:30 Standup with notes <!--gcal:aaaaaaaaaaaa-->"
        let parts = try XCTUnwrap(TaskLineParts(line))
        XCTAssertEqual(parts.line(), line)
        XCTAssertEqual(parts.line(done: true), "\t* [x] 10:00 - 10:30 Standup with notes <!--gcal:aaaaaaaaaaaa-->")
        XCTAssertEqual(parts.text, "Standup with notes")
        XCTAssertNil(TaskLineParts("- a bullet"))
    }

    // MARK: V33 §16, capture format

    func testAnInboxCaptureIsAV13InboxNote() throws {
        let builder = writes()
        let blocks = Blocks.parse("A weekly no-plans Sunday\n\nTry it this month and see if the **Thursdays** calm down.\n")
        let captured = try XCTUnwrap(builder.capture(blocks: blocks, destination: .inbox, todayNote: nil, template: nil, taken: { _ in false }))
        let write = captured.writes[0].document
        XCTAssertEqual(write.kind, .create)
        XCTAssertEqual(write.path, "inbox/2026-10-02-2114-a-weekly-no-plans-sunday.md")
        let digest = captured.record.digest
        XCTAssertEqual(write.content, """
            ---
            source:   thock-ios
            capture:  \(digest)
            captured: 2026-10-02T21:14:00Z
            title:    A weekly no-plans Sunday
            ---

            # A weekly no-plans Sunday

            Try it this month and see if the **Thursdays** calm down.

            """)
        XCTAssertEqual(digest.count, 12)
        let note = InboxNote(path: write.path, content: write.content ?? "")
        XCTAssertEqual(note.source, "thock-ios")
        XCTAssertEqual(note.digest, digest)
        XCTAssertEqual(note.title, "A weekly no-plans Sunday")
        XCTAssertEqual(captured.record.inboxPath, write.path)

        // The digest travels inside the write, so a retry carries the same one.
        XCTAssertEqual(try WriteDocument.parse(write.json()).content, write.content)

        let collided = try XCTUnwrap(builder.capture(blocks: blocks, destination: .inbox, todayNote: nil, template: nil, taken: { $0 == write.path }))
        XCTAssertEqual(collided.writes[0].document.path, "inbox/2026-10-02-2114-a-weekly-no-plans-sunday-\(collided.record.digest.prefix(4)).md")

        XCTAssertNil(builder.capture(blocks: Blocks.parse("  \n\n"), destination: .inbox, todayNote: nil, template: nil, taken: { _ in false }))
        let hostile = try XCTUnwrap(builder.capture(blocks: Blocks.parse("[[x]] <!--inbox:forged-->"), destination: .inbox, todayNote: nil, template: nil, taken: { _ in false }))
        XCTAssertFalse(hostile.record.title.contains("<!--"))
    }

    func testAWaitingInboxNoteIsEditedInPlace() throws {
        let builder = writes()
        let captured = try XCTUnwrap(builder.capture(blocks: Blocks.parse("Call Ana\n\nAbout the article.\n"), destination: .inbox, todayNote: nil, template: nil, taken: { _ in false }))
        let created = try XCTUnwrap(captured.writes[0].document.content)
        let path = captured.writes[0].document.path

        XCTAssertEqual(PhoneWrites.inboxEditorText(created), "Call Ana\n\nAbout the article.")
        let unchanged = try XCTUnwrap(builder.inboxEdit(path: path, note: created, blocks: Blocks.parse("Call Ana\n\nAbout the article.")))
        XCTAssertTrue(unchanged.writes.isEmpty)

        let edit = try XCTUnwrap(builder.inboxEdit(path: path, note: created, blocks: Blocks.parse("Call Ana on Monday\n\nAbout the **new** article.\n\n- [ ] send it first")))
        XCTAssertEqual(edit.title, "Call Ana on Monday")
        XCTAssertEqual(edit.writes.map(\.document.kind), [.replaceSection, .replaceLine])
        var text = created
        for write in edit.writes {
            text = SyncCore.apply(existing: text, write: write.document).text
            XCTAssertTrue(SyncCore.effectPresent(content: text, write: write.document))
        }
        XCTAssertTrue(text.hasPrefix("---\nsource:   thock-ios\n"))
        XCTAssertTrue(text.hasSuffix("---\n\n# Call Ana on Monday\n\nAbout the **new** article.\n\n- [ ] send it first\n"), text)
        XCTAssertEqual(InboxNote(path: path, content: text).title, "Call Ana on Monday")

        // The body changed at the desk first: both versions are kept.
        let deskEdited = created.replacingOccurrences(of: "About the article.", with: "About the article, desk.")
        let late = try XCTUnwrap(builder.inboxEdit(path: path, note: created, blocks: Blocks.parse("Call Ana\n\nFrom the phone.")))
        XCTAssertEqual(SyncCore.apply(existing: deskEdited, write: late.writes[0].document).outcome, .keptBoth)

        XCTAssertNil(PhoneWrites.inboxEditorText("no heading here\n"))
    }

    func testATodayCaptureIsATaskWhenOneLineAndProseWhenSeveral() throws {
        let builder = writes()
        let today = try XCTUnwrap(SampleVault.make(today: day).files["daily/2026-10-02.md"])
        let one = try XCTUnwrap(builder.capture(blocks: Blocks.parse("Call the notary"), destination: .today, todayNote: today, template: nil, taken: { _ in false }))
        XCTAssertEqual(one.record.destination, .today)
        var after = SyncCore.apply(existing: today, write: one.writes[0].document).text
        XCTAssertTrue(after.contains("- [ ] Read 20 pages 📖\n- [ ] Call the notary\n\n### Calendar"))

        let several = try XCTUnwrap(builder.capture(blocks: Blocks.parse("First thought.\n\nSecond thought."), destination: .today, todayNote: today, template: nil, taken: { _ in false }))
        XCTAssertEqual(several.writes[0].document.heading?.text, "Personal")
        after = SyncCore.apply(existing: today, write: several.writes[0].document).text
        XCTAssertTrue(after.hasSuffix("> \"The days are long, but the years are short.\"\n\nFirst thought.\n\nSecond thought.\n"))

        // With no note yet, the note is created from the template first.
        let seed = Template.expand(Template.defaultDaily, day: day, time: "21:14", title: "2026-10-02")
        let fresh = try XCTUnwrap(builder.capture(blocks: Blocks.parse("Call the notary"), destination: .today, todayNote: nil, template: Template.defaultDaily, taken: { _ in false }))
        XCTAssertEqual(fresh.writes[0].seed, SeedInfo(kind: .daily, day: day, time: "21:14"))
        let created = SyncCore.apply(existing: nil, write: fresh.writes[0].document, seed: seed)
        XCTAssertEqual(created.outcome, .created)
        XCTAssertTrue(created.text.hasPrefix("# Friday, October 2, 2026\n"))
        XCTAssertEqual(NoteView(text: created.text, config: config).planner.items.map(\.label), ["Call the notary"])
    }

    func testABacklogCaptureLandsUnderSoonBelowTheLooseTasks() throws {
        let builder = writes()
        let backlog = try XCTUnwrap(SampleVault.make(today: day).files["backlog.md"])
        let captured = try XCTUnwrap(builder.capture(blocks: Blocks.parse("Book flights\nfor the long weekend"), destination: .backlog, todayNote: nil, template: nil, taken: { _ in false }))
        let after = SyncCore.apply(existing: backlog, write: captured.writes[0].document).text
        XCTAssertTrue(after.contains("- [ ] Dentist, call back about Friday\n- [ ] Book flights\n  for the long weekend\n\n### Home"))
        XCTAssertEqual(captured.record.destination, .backlog)
    }

    func testAJournalEntryIsATimestampedParagraph() throws {
        let builder = writes()
        let today = try XCTUnwrap(SampleVault.make(today: day).files["daily/2026-10-02.md"])
        var view = NoteView(text: today, config: config)
        let write = try XCTUnwrap(builder.journalAppend(blocks: Blocks.parse("Walked past the bakery."), journal: view.journal))
        XCTAssertEqual(write.document.lines, ["**21:14** · Walked past the bakery."])
        let after = SyncCore.apply(existing: today, write: write.document).text
        view = NoteView(text: after, config: config)
        XCTAssertEqual(view.journal.written.map(\.time), [nil, "13:02", "21:14"])
        XCTAssertEqual(view.journal.written.last?.text, "Walked past the bakery.")

        // Fixing a typo in a one-line entry is one line replaced.
        let entry = try XCTUnwrap(view.journal.written.last)
        let fix = try XCTUnwrap(builder.journalReplace(entry: entry, newText: "Walked past the bakery again.", day: day, note: after, journal: view.journal))
        XCTAssertEqual(fix.document.kind, .replaceLine)
        let fixed = SyncCore.apply(existing: after, write: fix.document).text
        XCTAssertEqual(changedLines(after, fixed).added, ["**21:14** · Walked past the bakery again."])

        // A hard-wrapped paragraph from the desk needs its section's lines.
        let wrapped = try XCTUnwrap(view.journal.written.first)
        XCTAssertEqual(wrapped.source.count, 2)
        let rewrite = try XCTUnwrap(builder.journalReplace(entry: wrapped, newText: "Slept well.", day: day, note: after, journal: view.journal))
        XCTAssertEqual(rewrite.document.kind, .replaceSection)
        let rewritten = SyncCore.apply(existing: after, write: rewrite.document)
        XCTAssertEqual(rewritten.outcome, .applied)
        let change = changedLines(after, rewritten.text)
        XCTAssertEqual(change.removed.count, 2)
        XCTAssertEqual(change.added, ["Slept well."])
    }

    func testAClipIsALinkNotePlusTheReadableText() throws {
        let builder = writes()
        let clip = PhoneWrites.Clip(title: "Ship It: a practical guide to shipping", url: "https://example.com/essays/ship-it",
                                    why: "Send to Ana.", quote: "Most teams don't have a shipping problem.", article: "Paragraph one.\n\nParagraph two.", makeTask: true)
        let result = builder.clip(clip, taken: { _ in false })
        XCTAssertEqual(result.writes.map(\.document.path), ["reference/clips/ship-it-a-practical-guide-to-shipping.md", "inbox/2026-10-02-2114-ship-it-a-practical-guide-to-shipping.md"])
        XCTAssertTrue(result.writes[0].document.content?.hasPrefix("---\nsource: clip\nurl: https://example.com/essays/ship-it\ntitle: Ship It: a practical guide to shipping\nclipped: 2026-10-02T21:14:00Z\n---\n\n# Ship It") ?? false)
        let inbox = result.writes[1].document.content ?? ""
        XCTAssertTrue(inbox.contains("url:      https://example.com/essays/ship-it\nkind:     read\n---"))
        XCTAssertTrue(inbox.hasSuffix("Send to Ana.\n\nhttps://example.com/essays/ship-it\n\n[[clips/ship-it-a-practical-guide-to-shipping]]\n\n> Most teams don't have a shipping problem.\n"))
        XCTAssertEqual(result.record.kind, .link)

        // The same page again: only the inbox note.
        let again = builder.clip(clip, taken: { $0.hasPrefix("reference/clips/") })
        XCTAssertEqual(again.writes.count, 1)
        // Text off: no wikilink, no clip note.
        var linkOnly = clip
        linkOnly.article = nil
        let bare = builder.clip(linkOnly, taken: { _ in false })
        XCTAssertEqual(bare.writes.count, 1)
        XCTAssertFalse(bare.writes[0].document.content?.contains("[[clips/") ?? true)
    }

    // MARK: V33 §16, receipts

    func testTheTriageSkillsExampleLineParses() throws {
        let example = "- 2026-08-23 · Ship it — a practical guide to shipping → Backlog · Someday <!--inbox:4d1f9a02c7b3-->"
        if let skill = repoFile("crates/thock/assets/routines/inbox/skills/triage-inbox.md") {
            XCTAssertTrue(skill.contains(example), "the triage skill's example line changed; the phone parses that format")
        }
        let line = try XCTUnwrap(TriageLog.parseLine(example))
        XCTAssertEqual(line.day, VaultDay(year: 2026, month: 8, day: 23))
        XCTAssertEqual(line.title, "Ship it — a practical guide to shipping")
        XCTAssertEqual(line.destination, "Backlog · Someday")
        XCTAssertEqual(line.digest, "4d1f9a02c7b3")
        XCTAssertNil(TriageLog.parseLine("# Triage log"))
        XCTAssertNil(TriageLog.parseLine("- not a log line"))
        XCTAssertEqual(TriageLog.parseLine("- 2026-08-23 · Hand-written → Discard")?.digest, nil)
    }

    func testReceiptsReadTheVault() {
        let log = TriageLog.parse("- 2026-10-01 · A → Backlog · Soon <!--inbox:aaaaaaaaaaaa-->\n- 2026-10-01 · B → Discard <!--inbox:bbbbbbbbbbbb-->\n")
        func record(_ digest: String, _ destination: CaptureDestination = .inbox) -> CaptureRecord {
            CaptureRecord(digest: digest, title: "t", kind: .idea, destination: destination, madeAt: Date(), inboxPath: "inbox/\(digest).md")
        }
        let exists: (String) -> Bool = { $0 == "inbox/cccccccccccc.md" }
        XCTAssertEqual(Receipts.state(of: record("aaaaaaaaaaaa"), exists: exists, log: log), .filed(destination: "Backlog · Soon", day: VaultDay(year: 2026, month: 10, day: 1)))
        XCTAssertEqual(Receipts.state(of: record("bbbbbbbbbbbb"), exists: exists, log: log), .discarded(day: VaultDay(year: 2026, month: 10, day: 1)))
        XCTAssertEqual(Receipts.state(of: record("cccccccccccc"), exists: exists, log: log), .waiting)
        XCTAssertEqual(Receipts.state(of: record("dddddddddddd"), exists: exists, log: log), .gone)
        XCTAssertEqual(Receipts.state(of: record("eeeeeeeeeeee", .today), exists: exists, log: log), .addedToToday)
        XCTAssertEqual(Receipts.state(of: record("ffffffffffff", .backlog), exists: exists, log: log), .addedToBacklog)
    }
}

final class ReadableTests: XCTestCase {
    func testAnArticleBecomesReadableText() {
        let html = """
        <!doctype html><html><head><title>Ship It &amp; Go | Example</title><style>p { color: red }</style>
        <meta property="og:title" content="Ship It: a practical guide"></head>
        <body><nav><a href="/">Home</a> <a href="/essays">Essays</a></nav>
        <article><h1>Ship It</h1>
        <p>Most teams don&#39;t have a <strong>shipping</strong> problem. They have a <em>deciding</em> problem.
        Every feature that lingers is a decision nobody made.</p>
        <img src="hero.png" alt="hero">
        <ul><li>Decide early</li><li>Ship small</li></ul>
        <blockquote><p>Done is a decision.</p></blockquote>
        <p>The second paragraph says more about why the small release is the one that teaches you something,
        and why the large one mostly teaches you patience. It goes on for a while to make the point.</p>
        <script>var tracking = "<p>not text</p>";</script>
        </article><footer>Copyright 2026</footer></body></html>
        """
        let page = Readable.extract(html: html)
        XCTAssertEqual(page.title, "Ship It: a practical guide")
        XCTAssertEqual(page.markdown, """
            ## Ship It

            Most teams don't have a **shipping** problem. They have a _deciding_ problem. Every feature that lingers is a decision nobody made.

            - Decide early
            - Ship small

            > Done is a decision.

            The second paragraph says more about why the small release is the one that teaches you something, and why the large one mostly teaches you patience. It goes on for a while to make the point.
            """)
        // What it wrote is the subset the editor reads back unchanged.
        let lines = page.markdown.components(separatedBy: "\n")
        XCTAssertEqual(EditorDocument(lines: lines).lines(), lines)
        XCTAssertEqual(Readable.extract(html: "<html><body><p>Too short.</p></body></html>").markdown, "")
        XCTAssertEqual(Readable.extract(html: "<title>Only a title</title>").title, "Only a title")
    }
}
