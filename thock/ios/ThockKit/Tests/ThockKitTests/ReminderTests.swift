import XCTest
@testable import ThockKit

/// The phone's reminder model (V45 §10): the index read through to its home
/// lines, the settings, and every write the phone makes for a reminder.
final class ReminderTests: XCTestCase {
    var calendar: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC") ?? .gmt
        return calendar
    }

    /// Wednesday 21 October 2026, 14:05 UTC.
    let now = Date(timeIntervalSince1970: 1_792_591_500)
    let today = VaultDay(year: 2026, month: 10, day: 21)

    func vault(_ notes: [String: String]) throws -> VaultSession {
        let store = try VaultStore(url: nil)
        store.setMeta("vault_id", "vault")
        store.setMeta("device_id", "c41a9e0d7b2f4a61")
        for (index, note) in notes.sorted(by: { $0.key < $1.key }).enumerated() {
            try store.applySnapshot(path: note.key, version: index + 1, content: note.value, contentHash: "hash", blobID: "blob")
        }
        return VaultSession(store: store, calendar: calendar)
    }

    var writes: PhoneWrites {
        PhoneWrites(config: VaultConfig(), deviceID: "c41a9e0d7b2f4a61", now: now, calendar: calendar)
    }

    func due(_ text: String) throws -> ReminderDue {
        try XCTUnwrap(ReminderDue(text))
    }

    /// Applies `writes` in order, as the desk would, to the notes in `files`.
    func applied(_ writes: [PlannedWrite], to files: [String: String]) -> [String: String] {
        var files = files
        for write in writes {
            let path = write.document.path
            files[path] = SyncCore.apply(existing: files[path], write: write.document).text
        }
        return files
    }

    func testTheCalendarFixtureIsWednesday() {
        XCTAssertEqual(VaultDay(now, calendar: calendar), today)
        XCTAssertEqual(writes.clock, "14:05")
    }

    // MARK: The index

    func testEveryIndexFixtureReadsAsTheDeskReadsIt() throws {
        struct Headings: Decodable {
            var open: [String]
            var done: [String]
            var day: [String]
        }
        struct Daily: Decodable {
            var dir: String
            var filename: String
        }
        struct Stamp: Decodable {
            var kind: String
            var date: String
        }
        struct Expected: Decodable {
            var id: String
            var section: String
            var entry: String
            var stamp: Stamp?
            var home: String
            var line: Int?
            var text: String?
            var due: String?
            var rings_at: String?
            var state: String
            var group: String?
        }
        struct Case: Decodable {
            var name: String
            var now: String
            var index: String
            var headings: Headings
            var daily: Daily
            var date_only_at: String
            var files: [String: String]
            var reminders: [Expected]
        }
        var cases = 0
        for root in SyncCoreFixtureTests.fixtureRoots() {
            let folder = root.appendingPathComponent("reminder_index")
            guard let names = try? FileManager.default.contentsOfDirectory(atPath: folder.path) else { continue }
            for name in names.sorted() where name.hasSuffix(".json") {
                let vector = try JSONDecoder().decode(Case.self, from: Data(contentsOf: folder.appendingPathComponent(name)))
                var config = VaultConfig()
                config.daily.dir = vector.daily.dir
                config.daily.filename = vector.daily.filename
                config.reminders.index = vector.index
                config.reminders.openHeadings = vector.headings.open
                config.reminders.doneHeadings = vector.headings.done
                config.reminders.dayHeadings = vector.headings.day
                config.reminders.dateOnlyAt = try XCTUnwrap(ClockTime(vector.date_only_at))
                let today = try XCTUnwrap(VaultDay(iso: String(vector.now.prefix(10))))
                let index = ReminderIndex(config: config) { vector.files[$0] }
                XCTAssertEqual(index.reminders.map(\.id), vector.reminders.map(\.id), "\(vector.name): one reminder per entry, in file order")
                for (reminder, expected) in zip(index.reminders, vector.reminders) {
                    let label = "\(vector.name) \(expected.id)"
                    XCTAssertEqual(reminder.section.rawValue, expected.section, "\(label): section")
                    XCTAssertEqual(reminder.link == nil ? "loose" : "link", expected.entry, "\(label): entry")
                    let stamp: String? = reminder.stamp.map {
                        switch $0 {
                        case .done(let day): return "done \(day.iso)"
                        case .dropped(let day): return "dropped \(day.iso)"
                        case .removed(let day): return "removed \(day.iso)"
                        }
                    }
                    XCTAssertEqual(stamp, expected.stamp.map { "\($0.kind) \($0.date)" }, "\(label): stamp")
                    XCTAssertEqual(reminder.home, expected.home, "\(label): home")
                    XCTAssertEqual(reminder.line, expected.line, "\(label): line")
                    XCTAssertEqual(reminder.text, expected.text, "\(label): text")
                    XCTAssertEqual(reminder.due?.description, expected.due, "\(label): due")
                    let rings = reminder.due.map { due -> String in
                        let moment = due.ringsAt(dateOnlyAt: config.reminders.dateOnlyAt)
                        return "\(moment.date.iso)T\(moment.time)"
                    }
                    XCTAssertEqual(rings, expected.rings_at, "\(label): rings at")
                    XCTAssertEqual(reminder.status.rawValue, expected.state, "\(label): state")
                    XCTAssertEqual(reminder.group(today: today)?.rawValue, expected.group, "\(label): group")
                }
                cases += 1
            }
        }
        XCTAssertGreaterThanOrEqual(cases, 2)
    }

    func testGroupsSortSoonestFirstAndDoneMostRecentFirst() throws {
        let session = try vault([
            "reminders.md": """
            ## Open
            - [ ] Later today (@2026-10-21 18:00) <!--rem:000000000001-->
            - [ ] This morning (@2026-10-21 08:00) <!--rem:000000000002-->
            - [ ] All day (@2026-10-21) <!--rem:000000000003-->
            - [ ] Next week (@2026-10-26 09:00) <!--rem:000000000004-->
            - [x] Done first (@2026-10-19) <!--rem:000000000005-->
            - [x] Done second (@2026-10-20) <!--rem:000000000006-->

            """,
        ])
        let index = session.reminderIndex()
        let dateOnlyAt = session.config.reminders.dateOnlyAt
        XCTAssertEqual(index.reminders(in: .today, today: today, dateOnlyAt: dateOnlyAt).map(\.label), ["This morning", "All day", "Later today"])
        XCTAssertEqual(index.reminders(in: .upcoming, today: today, dateOnlyAt: dateOnlyAt).map(\.label), ["Next week"])
        XCTAssertEqual(index.reminders(in: .done, today: today, dateOnlyAt: dateOnlyAt).map(\.label), ["Done second", "Done first"])
        XCTAssertTrue(index.reminders(in: .overdue, today: today, dateOnlyAt: dateOnlyAt).isEmpty)
    }

    func testAVaultWithoutAnIndexHasNoReminders() throws {
        XCTAssertTrue(try vault([:]).reminderIndex().reminders.isEmpty)
    }

    // MARK: Settings

    func testTheSettingsFileIsReadAsTheDeskReadsIt() {
        let settings = ReminderSettings("""
        # Where the index lives.
        index = "lembretes.md"

        [headings]
        open = ["Abertos", "Open"]
        done = [" "]
        day = ["Lembretes"]

        [ring]
        date_only_at = "08:30"    # comment
        snooze_minutes = 30
        tomorrow_at = "07:45"
        desk = false
        unknown = 1
        """)
        XCTAssertEqual(settings.index, "lembretes.md")
        XCTAssertEqual(settings.openHeadings, ["Abertos", "Open"])
        XCTAssertEqual(settings.doneHeadings, ["Done"], "a list of blank names falls back")
        XCTAssertEqual(settings.dayHeadings, ["Lembretes"])
        XCTAssertEqual(settings.dateOnlyAt, ClockTime("08:30"))
        XCTAssertEqual(settings.snoozeMinutes, 30)
        XCTAssertEqual(settings.tomorrowAt, ClockTime("07:45"))

        let defaults = ReminderSettings(nil)
        XCTAssertEqual(defaults, ReminderSettings())
        XCTAssertEqual(defaults.index, "reminders.md")
        XCTAssertEqual(defaults.dateOnlyAt, ClockTime("09:00"))

        let bad = ReminderSettings("index = \"../outside.md\"\n[ring]\ndate_only_at = \"9am\"\nsnooze_minutes = 0\n")
        XCTAssertEqual(bad.index, "reminders.md")
        XCTAssertEqual(bad.dateOnlyAt, ClockTime("09:00"))
        XCTAssertEqual(bad.snoozeMinutes, 1, "clamped as the desk clamps it")

        let config = VaultConfig(config: "[daily]\ndir = \"journal\"\nfilename = \"YYYYMMDD\"\n", remindersConfig: "[headings]\nday = [\"Lembretes\"]\n")
        XCTAssertEqual(config.reminderConfig, ReminderConfig(daily: DailyNotes(dir: "journal", filename: "YYYYMMDD"), dayHeadings: ["Lembretes"]))
        XCTAssertEqual(config.day(ofDailyPath: "journal/20261021.md"), today)
    }

    func testTheQuickChoicesAreTheDesks() {
        let settings = ReminderSettings()
        XCTAssertEqual(settings.tomorrowDue(today: today).description, "2026-10-22 09:00")
        XCTAssertEqual(settings.mondayDue(today: today).description, "2026-10-26 09:00")
        XCTAssertEqual(settings.mondayDue(today: VaultDay(year: 2026, month: 10, day: 26)).description, "2026-11-02 09:00", "never today")
        XCTAssertEqual(settings.mondayDue(today: VaultDay(year: 2026, month: 10, day: 25)).description, "2026-10-26 09:00")
        XCTAssertEqual(settings.snoozedDue(now: now, calendar: calendar).description, "2026-10-21 15:05")
        XCTAssertEqual(settings.snoozedDue(now: now.addingTimeInterval(20), calendar: calendar).description, "2026-10-21 15:06", "rounded up to the minute")
    }

    // MARK: Capture

    func testAReminderCaptureIsOneAppendUnderOpen() throws {
        let blocks = Blocks.parse("Call the bank\nabout the card\n\n- the old one")
        let captured = try XCTUnwrap(writes.capture(blocks: blocks, destination: .reminder, reminder: ReminderCapture(due: try due("2026-10-22 10:00"), id: "3f9a1c0d4e21"), todayNote: nil, template: nil, taken: { _ in false }))
        XCTAssertEqual(captured.writes.count, 1)
        let write = try XCTUnwrap(captured.writes.first?.document)
        XCTAssertEqual(write.kind, .append)
        XCTAssertEqual(write.path, "reminders.md")
        XCTAssertEqual(write.heading, HeadingRef(text: "Open", level: 2))
        XCTAssertEqual(write.lines, ["- [ ] Call the bank (@2026-10-22 10:00) <!--rem:3f9a1c0d4e21-->", "  about the card", "  - the old one"])
        XCTAssertEqual(captured.record.destination, .reminder)

        let files = applied(captured.writes, to: ["reminders.md": "# Reminders\n\n## Open\n- [[backlog]] <!--rem:2f6b08e19a5c-->\n\n## Done\n"])
        XCTAssertEqual(files["reminders.md"], "# Reminders\n\n## Open\n- [[backlog]] <!--rem:2f6b08e19a5c-->\n- [ ] Call the bank (@2026-10-22 10:00) <!--rem:3f9a1c0d4e21-->\n  about the card\n  - the old one\n\n## Done\n")
        let missing = applied(captured.writes, to: [:])
        XCTAssertEqual(missing["reminders.md"], "## Open\n- [ ] Call the bank (@2026-10-22 10:00) <!--rem:3f9a1c0d4e21-->\n  about the card\n  - the old one\n\n", "a missing index gets the heading and the line")

        XCTAssertNil(writes.capture(blocks: blocks, destination: .reminder, todayNote: nil, template: nil, taken: { _ in false }), "a reminder needs its time")
    }

    func testTheSessionDrawsAFreshIdAndRecordsTheCapture() throws {
        let session = try vault(["reminders.md": "## Open\n"])
        let record = try XCTUnwrap(session.capture(blocks: Blocks.parse("Send the invoice"), destination: .reminder, due: try due("2026-10-23"), now: now))
        XCTAssertEqual(record.destination, .reminder)
        let line = try XCTUnwrap(session.store.content("reminders.md")?.components(separatedBy: "\n").first { $0.contains("Send the invoice") })
        let reminder = try XCTUnwrap(Reminders.parseLine(line, day: nil))
        XCTAssertEqual(reminder.due?.description, "2026-10-23")
        XCTAssertNotNil(reminder.id)
        XCTAssertEqual(session.reminderIndex().reminders.first?.label, "Send the invoice")
        XCTAssertEqual(Receipts.state(of: record, exists: { _ in false }, log: []), .addedToReminders)
    }

    // MARK: Remind me…

    func testRemindMeOnAPlannerLineWritesTheTokenAndALink() throws {
        let note = "# Wed\n\n## Day planner\n- [ ] 16:00 - 17:00 Dentist\n- [ ] Call Ana <!--gmail:ab12-->\n"
        let view = NoteView(text: note, config: VaultConfig(), day: today)
        let dentist = try XCTUnwrap(view.planner.items.first)
        let writes = try XCTUnwrap(self.writes.remindMe(raw: dentist.raw, path: "daily/2026-10-21.md", heading: dentist.heading, hash: dentist.hash, ordinal: dentist.ordinal, due: try due("2026-10-21 15:30"), id: "5b71c08e3f6d"))
        XCTAssertEqual(writes.map(\.document.kind), [.replaceLine, .append])
        XCTAssertEqual(writes[0].document.newLine, "- [ ] 16:00 - 17:00 Dentist (@15:30) <!--rem:5b71c08e3f6d-->", "a time on its own day's note")
        XCTAssertEqual(writes[1].document.path, "reminders.md")
        XCTAssertEqual(writes[1].document.lines, ["- [[daily/2026-10-21]] <!--rem:5b71c08e3f6d-->"])

        let ana = try XCTUnwrap(view.planner.items.last)
        let other = try XCTUnwrap(self.writes.remindMe(raw: ana.raw, path: "daily/2026-10-21.md", heading: ana.heading, hash: ana.hash, ordinal: ana.ordinal, due: try due("2026-10-22 09:00"), id: "7d3e55a1c0f2"))
        XCTAssertEqual(other[0].document.newLine, "- [ ] Call Ana (@2026-10-22 09:00) <!--gmail:ab12--> <!--rem:7d3e55a1c0f2-->", "another day's time is a full date; the id goes last")

        let files = applied(writes, to: ["daily/2026-10-21.md": note])
        XCTAssertEqual(files["daily/2026-10-21.md"], "# Wed\n\n## Day planner\n- [ ] 16:00 - 17:00 Dentist (@15:30) <!--rem:5b71c08e3f6d-->\n- [ ] Call Ana <!--gmail:ab12-->\n")
        XCTAssertEqual(files["reminders.md"], "## Open\n- [[daily/2026-10-21]] <!--rem:5b71c08e3f6d-->\n\n")
    }

    func testRemindMeOnAReminderReschedulesItWithoutANewLink() throws {
        let raw = "- [ ] Renew the car insurance (@2026-10-20 09:00) <!--rem:2f6b08e19a5c-->"
        let writes = try XCTUnwrap(self.writes.remindMe(raw: raw, path: "backlog.md", heading: HeadingRef(text: "Soon", level: 2), hash: SyncCore.lineHash(raw), ordinal: 0, due: try due("2026-10-26 09:00"), id: "999999999999"))
        XCTAssertEqual(writes.map(\.document.kind), [.replaceLine])
        XCTAssertEqual(writes[0].document.newLine, "- [ ] Renew the car insurance (@2026-10-26 09:00) <!--rem:2f6b08e19a5c-->")
        XCTAssertNil(self.writes.remindMe(raw: "- [ ] Standup <!--gcal:5d0c2a91b7e4-->", path: "daily/2026-10-21.md", heading: nil, hash: "x", ordinal: 0, due: try due("2026-10-22"), id: "999999999999"), "a calendar line is calendar sync's")
    }

    func testRemindMeOnABacklogTaskGoesThroughTheSession() throws {
        let session = try vault(["backlog.md": "# Backlog\n\n## Soon\n- [ ] Buy stamps\n", "reminders.md": "## Open\n- [ ] Old (@2026-10-01) <!--rem:aaaaaaaaaaaa-->\n"])
        let task = try XCTUnwrap(session.backlog().soon.tasks.first)
        try session.remindMe(task, due: try due("2026-10-24"), now: now)
        let line = try XCTUnwrap(session.store.content("backlog.md")?.components(separatedBy: "\n").first { $0.contains("Buy stamps") })
        let id = try XCTUnwrap(Reminders.parseLine(line, day: nil)?.id)
        XCTAssertNotEqual(id, "aaaaaaaaaaaa")
        XCTAssertEqual(line, "- [ ] Buy stamps (@2026-10-24) <!--rem:\(id)-->")
        XCTAssertTrue(session.store.content("reminders.md")?.contains("- [[backlog]] <!--rem:\(id)-->") ?? false)
        let indexed = try XCTUnwrap(session.reminderIndex().reminder(id: id))
        XCTAssertEqual(indexed.status, .open)
        XCTAssertEqual(session.backlog().soon.tasks.first?.label, "Buy stamps", "the label drops the token")
    }

    // MARK: Home-line edits

    static let house = """
    # House

    - [ ] Call the plumber (@2026-10-21 14:30) <!--rem:7d3e55a1c0f2-->
    The boiler ticks. Ask about it (@2026-11-02) <!--rem:7c11e0b4d2a9-->
    - [ ] Call the plumber (@2026-10-21 14:30)

    """

    func houseIndex() -> ReminderIndex {
        ReminderIndex(config: VaultConfig()) { path in
            [
                "reminders.md": "## Open\n- [[projects/house]] <!--rem:7d3e55a1c0f2-->\n- [[projects/house]] <!--rem:7c11e0b4d2a9-->\n",
                "projects/house.md": Self.house,
            ][path]
        }
    }

    func testEachGestureIsOneReplaceLineOnTheHomeLine() throws {
        let index = houseIndex()
        let plumber = try XCTUnwrap(index.reminder(id: "7d3e55a1c0f2"))
        let boiler = try XCTUnwrap(index.reminder(id: "7c11e0b4d2a9"))
        let cases: [(PlannedWrite?, String)] = [
            (writes.closeReminder(plumber, note: Self.house), "- [x] Call the plumber (@2026-10-21 14:30) <!--rem:7d3e55a1c0f2-->"),
            (writes.closeReminder(boiler, note: Self.house), "The boiler ticks. Ask about it ~~(@2026-11-02)~~ <!--rem:7c11e0b4d2a9-->"),
            (writes.rescheduleReminder(plumber, to: try due("2026-10-22 09:00"), note: Self.house), "- [ ] Call the plumber (@2026-10-22 09:00) <!--rem:7d3e55a1c0f2-->"),
            (writes.dropReminder(plumber, note: Self.house), "- [ ] Call the plumber <!--rem:7d3e55a1c0f2-->"),
        ]
        for (write, line) in cases {
            let document = try XCTUnwrap(write?.document)
            XCTAssertEqual(document.kind, .replaceLine)
            XCTAssertEqual(document.path, "projects/house.md")
            XCTAssertNil(document.heading)
            XCTAssertEqual(document.newLine, line)
            let after = SyncCore.apply(existing: Self.house, write: document).text
            XCTAssertTrue(after.contains(line + "\n"), line)
            // The plain twin below, which has the same words, is left alone.
            XCTAssertTrue(after.hasSuffix("- [ ] Call the plumber (@2026-10-21 14:30)\n"), line)
        }
        XCTAssertNil(writes.reopenReminder(plumber, note: Self.house), "an open reminder has nothing to reopen")
    }

    func testAClosedReminderReopens() throws {
        let session = try vault(["reminders.md": "## Done\n- [x] Pay the nursery (@2026-10-19 09:00) ✅ 2026-10-19 <!--rem:6e1f42b07a93-->\n"])
        let reminder = try XCTUnwrap(session.reminderIndex().reminders.first)
        XCTAssertEqual(reminder.label, "Pay the nursery")
        try session.reopenReminder(reminder, now: now)
        XCTAssertEqual(session.reminderIndex().reminders.first?.status, .open)
    }

    // MARK: Lines that carry a reminder elsewhere

    func testPlannerLabelsDropTokensAndMirrorsReadAsTheirText() throws {
        let note = """
        # Wed

        ## Day planner
        - [ ] 16:00 - 17:00 Dentist (@15:30) <!--rem:5b71c08e3f6d-->

        ### Reminders
        - [ ] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->
        - [ ] Renew the car insurance · 2026-10-20 09:00 · [[backlog]] <!--rem:2f6b08e19a5c-->
        - [ ] ~~Old thing~~ → 2026-10-23 · [[backlog]] <!--rem:111111111111-->

        """
        let planner = NoteView(text: note, config: VaultConfig(), day: today).planner
        let items = planner.items
        XCTAssertEqual(items.map(\.label), ["Dentist", "Call the plumber", "Renew the car insurance", "Old thing"])
        XCTAssertEqual(items[0].reminder?.due?.description, "2026-10-21 15:30", "a time-only token takes the note's day")
        XCTAssertEqual(items[0].time?.raw, "16:00 - 17:00")
        XCTAssertNil(items[0].mirror)
        XCTAssertEqual(items[1].time?.raw, "14:30")
        XCTAssertEqual(items[1].mirror?.note, "projects/house")
        XCTAssertNil(items[2].time)
        XCTAssertEqual(items[2].mirror?.carried?.description, "2026-10-20 09:00")
        XCTAssertTrue(items[3].struck)
        XCTAssertEqual(ReminderChip(try XCTUnwrap(items[0].reminder), today: today)?.label, "Today 15:30")
    }

    func testAnEditKeepsTheTokenAndTheId() throws {
        let note = "# Wed\n\n## Day planner\n- [ ] Call the plumber (@2026-10-22 09:00) <!--rem:7d3e55a1c0f2-->\n"
        let item = try XCTUnwrap(NoteView(text: note, config: VaultConfig(), day: today).planner.items.first)
        XCTAssertEqual(item.label, "Call the plumber")
        let edit = try XCTUnwrap(writes.editText(item, text: "Call the electrician", note: .day(today)))
        XCTAssertEqual(edit.document.newLine, "- [ ] Call the electrician (@2026-10-22 09:00) <!--rem:7d3e55a1c0f2-->")
        let tick = try XCTUnwrap(writes.tick(item, note: .day(today)))
        XCTAssertEqual(tick.document.newLine, "- [x] Call the plumber (@2026-10-22 09:00) <!--rem:7d3e55a1c0f2-->")
        let time = try XCTUnwrap(writes.setTime(item, startMinutes: 600, endMinutes: nil, note: .day(today)))
        XCTAssertEqual(time.document.newLine, "- [ ] 10:00 Call the plumber (@2026-10-22 09:00) <!--rem:7d3e55a1c0f2-->")

        // A token in the middle of the words stays where it was written.
        let middle = try XCTUnwrap(TaskLineParts("- [ ] Ask (@2026-11-02) about the boiler <!--rem:7c11e0b4d2a9-->"))
        XCTAssertNil(middle.token)
        XCTAssertEqual(middle.line(done: true), "- [x] Ask (@2026-11-02) about the boiler <!--rem:7c11e0b4d2a9-->")
        let spaced = try XCTUnwrap(TaskLineParts("- [ ] Ask  (@2026-11-02)  <!--rem:7c11e0b4d2a9-->"))
        XCTAssertEqual(spaced.line(), "- [ ] Ask  (@2026-11-02)  <!--rem:7c11e0b4d2a9-->", "byte for byte")
    }

    func testMovingAReminderToSoonKeepsItOneReminder() throws {
        let note = "# Wed\n\n## Day planner\n- [ ] Call the plumber (@2026-10-22 09:00) <!--gmail:ab12--> <!--rem:7d3e55a1c0f2-->\n"
        let item = try XCTUnwrap(NoteView(text: note, config: VaultConfig(), day: today).planner.items.first)
        let moved = writes.moveToSoon(item, note: .day(today))
        XCTAssertEqual(moved.last?.document.lines, ["- [ ] Call the plumber (@2026-10-22 09:00) <!--rem:7d3e55a1c0f2-->"])
    }

    func testTheBacklogStampGoesBeforeTheTrailingComments() throws {
        let backlog = "# Backlog\n\n## Soon\n- [ ] Renew the passport (@2026-12-02 09:00) <!--rem:2f6b08e19a5c-->\n\n## Completed\n"
        let view = BacklogView(text: backlog, config: VaultConfig())
        let task = try XCTUnwrap(view.soon.tasks.first)
        XCTAssertEqual(task.label, "Renew the passport")
        XCTAssertEqual(task.reminder?.id, "2f6b08e19a5c")
        let ticked = writes.backlogTick(task, completed: view.completed, todayNote: nil, template: nil)
        XCTAssertEqual(ticked.first?.document.lines, ["- [x] Renew the passport"], "today's record is not a second reminder")
        XCTAssertEqual(ticked.last?.document.newLine, "- [x] Renew the passport (@2026-12-02 09:00) ✅ 2026-10-21 <!--rem:2f6b08e19a5c-->")

        let after = applied(ticked, to: ["backlog.md": backlog])
        let completed = try XCTUnwrap(BacklogView(text: after["backlog.md"] ?? "", config: VaultConfig()).completed.tasks.first)
        XCTAssertEqual(completed.completedOn, "2026-10-21")
        XCTAssertEqual(completed.label, "Renew the passport")
        let older = BacklogView(text: "## Completed\n- [x] Renew the passport ✅ 2026-10-20 (@2026-12-02) <!--rem:2f6b08e19a5c-->\n", config: VaultConfig())
        XCTAssertEqual(older.completed.tasks.first?.completedOn, "2026-10-20", "a stamp written before the token still reads")
    }

    // MARK: The chip

    func testChipLabelsAreTheDesks() throws {
        func chip(_ line: String, path: String = "notes/a.md") throws -> ReminderChip? {
            ReminderChip(try XCTUnwrap(Reminders.parseLine(line, day: VaultConfig().day(ofDailyPath: path))), today: today)
        }
        XCTAssertEqual(try chip("A (@2026-10-22 10:00)"), ReminderChip(state: .upcoming, label: "Tomorrow 10:00"))
        XCTAssertEqual(try chip("A (@2026-10-21 14:30)"), ReminderChip(state: .today, label: "Today 14:30"))
        XCTAssertEqual(try chip("A (@2026-10-20 18:00)"), ReminderChip(state: .overdue, label: "Yesterday 18:00 · overdue"))
        XCTAssertEqual(try chip("A (@2026-10-13 18:00)"), ReminderChip(state: .overdue, label: "Tue 13 Oct 18:00 · overdue"))
        XCTAssertEqual(try chip("- [x] A (@2026-10-19)"), ReminderChip(state: .closed, label: "Mon 19 Oct"))
        XCTAssertEqual(try chip("A (@2027-01-04)"), ReminderChip(state: .upcoming, label: "Mon 4 Jan 2027"))
        XCTAssertEqual(try chip("A (@17:00)"), ReminderChip(state: .noDay, label: "17:00 · which day?"))
        XCTAssertEqual(try chip("A (@17:00)", path: "daily/2026-10-21.md"), ReminderChip(state: .today, label: "Today 17:00"))
    }
}
