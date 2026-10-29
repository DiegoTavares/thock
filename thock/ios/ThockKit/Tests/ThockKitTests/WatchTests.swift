import SQLite3
import XCTest
@testable import ThockKit

/// The watch link (V48 §6, §7.3, §12): the phone's reading of the days, the
/// complication's timeline, and requests run through the phone's own calls.
final class WatchTests: XCTestCase {
    var calendar: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC") ?? .gmt
        return calendar
    }

    /// Wednesday 21 October 2026, 14:05 UTC.
    let now = Date(timeIntervalSince1970: 1_792_591_500)
    let today = VaultDay(year: 2026, month: 10, day: 21)

    let todayNote = """
        # Wed

        ## Day planner
        - [ ] 09:00 - 10:00 Write the brief
        - [x] Pay the rent
        - [ ] 16:00 - 17:00 Dentist (@15:30) <!--rem:5b71c08e3f6d-->
        - [ ] Buy **milk**
        A note among the tasks.

        ### Calendar
        - [ ] 13:00 - 14:30 Standup <!--gcal:9f2c1ab4e7d0-->
        - [ ] 15:00 - 15:30 Review <!--gcal:1f2c1ab4e7d0-->

        ### Reminders
        - [ ] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->
        - [ ] Renew the car insurance · 2026-10-20 09:00 · [[backlog]] <!--rem:2f6b08e19a5c-->

        """

    func vault(_ notes: [String: String], connected: Bool = true) throws -> VaultSession {
        let store = try VaultStore(url: nil)
        if connected {
            store.setMeta("vault_id", "vault")
        }
        store.setMeta("device_id", "c41a9e0d7b2f4a61")
        for (index, note) in notes.sorted(by: { $0.key < $1.key }).enumerated() {
            try store.applySnapshot(path: note.key, version: index + 1, content: note.value, contentHash: "hash", blobID: "blob")
        }
        return VaultSession(store: store, calendar: calendar)
    }

    // MARK: The snapshot

    func testPlannerGroupsBecomeRowsInPlannerOrder() throws {
        let session = try vault(["daily/2026-10-21.md": todayNote])
        let snapshot = session.watchSnapshot(now: now)
        XCTAssertEqual(snapshot.state, .ready)
        XCTAssertEqual(snapshot.today, "2026-10-21")
        XCTAssertEqual(snapshot.days.count, 15)
        XCTAssertEqual(snapshot.days.first?.day, "2026-10-14")
        XCTAssertEqual(snapshot.days.last?.day, "2026-10-28")
        let day = try XCTUnwrap(snapshot.day(today))
        XCTAssertTrue(day.exists)
        XCTAssertEqual(day.notes, 1)
        XCTAssertEqual(day.rows.map(\.label), ["Write the brief", "Pay the rent", "Dentist", "Buy milk", "Standup", "Review", "Call the plumber", "Renew the car insurance"])
        XCTAssertEqual(day.rows.map(\.kind), [.task, .task, .reminder, .task, .calendar, .calendar, .reminder, .reminder])
        XCTAssertEqual(day.rows[0].start, 540)
        XCTAssertEqual(day.rows[0].end, 600)
        XCTAssertTrue(day.rows[1].done)
        XCTAssertNil(day.rows[1].start)
        XCTAssertEqual(day.rows[2].start, 960, "the planner's time, not the token's")
        XCTAssertEqual(day.rows[6].start, 870)
        XCTAssertNil(day.rows[6].carriedFrom)
        XCTAssertEqual(day.rows[7].carriedFrom, "2026-10-20")
        XCTAssertNil(day.rows[7].start)
        XCTAssertFalse(day.rows.contains { $0.label.contains("rem:") || $0.label.contains("(@") || $0.label.contains("[[") })
        let items = NoteView(text: todayNote, config: VaultConfig(), day: today).planner.items
        XCTAssertEqual(day.rows[0].hash, items[0].hash)
        XCTAssertEqual(snapshot.reminders, WatchSnapshot.Reminders(dateOnlyAt: "09:00", tomorrowAt: "09:00", index: "reminders.md"))
    }

    func testAReminderLineWithOnlyATokenSitsAtItsTime() throws {
        let session = try vault(["daily/2026-10-21.md": "# Wed\n\n## Day planner\n- [ ] Dentist (@14:00) <!--rem:5b71c08e3f6d-->\n"])
        let row = try XCTUnwrap(session.watchSnapshot(now: now).day(today)?.rows.first)
        XCTAssertEqual(row.kind, .reminder)
        XCTAssertEqual(row.start, 840)
        XCTAssertNil(row.end)
        XCTAssertEqual(row.label, "Dentist")
    }

    func testADayWithNoNoteHasItsRemindersFromTheIndex() throws {
        let session = try vault([
            "reminders.md": "## Open\n- [[projects/house]] <!--rem:7d3e55a1c0f2-->\n- [[projects/house]] <!--rem:2f6b08e19a5c-->\n- [[projects/house]] <!--rem:3f6b08e19a5c-->\n",
            "projects/house.md": """
                # House

                - [ ] Call the plumber (@2026-10-22 18:30) <!--rem:7d3e55a1c0f2-->
                - [ ] Paint the **fence** (@2026-10-19) <!--rem:2f6b08e19a5c-->
                - [ ] Not yet (@2026-10-30) <!--rem:3f6b08e19a5c-->

                """,
        ])
        let snapshot = session.watchSnapshot(now: now)
        let tomorrow = try XCTUnwrap(snapshot.day(today.adding(days: 1)))
        XCTAssertFalse(tomorrow.exists)
        XCTAssertEqual(tomorrow.rows.map(\.label), ["Call the plumber"])
        XCTAssertEqual(tomorrow.rows.first?.kind, .reminder)
        XCTAssertEqual(tomorrow.rows.first?.start, 18 * 60 + 30)
        let todayRows = try XCTUnwrap(snapshot.day(today)?.rows)
        XCTAssertEqual(todayRows.map(\.label), ["Paint the fence"])
        XCTAssertEqual(todayRows.first?.carriedFrom, "2026-10-19")
        XCTAssertEqual(snapshot.day(today.adding(days: 2))?.rows, [])
    }

    func testANoteWithNoPlannerHasNoRowsAndNoError() throws {
        let session = try vault(["daily/2026-10-21.md": "# Wed\n\n## Journal\nWords.\n"])
        let day = try XCTUnwrap(session.watchSnapshot(now: now).day(today))
        XCTAssertTrue(day.exists)
        XCTAssertEqual(day.rows, [])
        XCTAssertEqual(day.notes, 0)
    }

    func testAReadOnlySessionYieldsReadOnly() throws {
        let session = try vault(["daily/2026-10-21.md": todayNote])
        session.store.setMeta("status", "lapsed")
        let snapshot = session.watchSnapshot(now: now)
        XCTAssertEqual(snapshot.state, .readOnly)
        XCTAssertEqual(snapshot.day(today)?.rows.count, 8, "it still shows what it has")
        XCTAssertEqual(try vault([:], connected: false).watchSnapshot(now: now).state, .notConnected)
    }

    func testTheChipIsNeverReminder() throws {
        let session = try vault([:])
        XCTAssertEqual(session.watchSnapshot(chip: .backlog, now: now).chip, .backlog)
        XCTAssertEqual(session.watchSnapshot(chip: .reminder, now: now).chip, .today)
    }

    // MARK: The timeline

    func row(_ label: String, _ kind: WatchRow.Kind = .task, start: Int? = nil, end: Int? = nil, done: Bool = false) -> WatchRow {
        WatchRow(hash: label, ordinal: 0, kind: kind, start: start, end: end, label: label, done: done)
    }

    func at(_ hour: Int, _ minute: Int) -> Date {
        calendar.date(from: DateComponents(year: 2026, month: 10, day: 21, hour: hour, minute: minute)) ?? now
    }

    func testEntriesEveryFifteenMinutesForFourHoursThenEveryHalfHour() {
        let entries = TimelineEntries.entries(rows: [], today: today, now: now, calendar: calendar)
        XCTAssertEqual(entries.first?.date, now)
        XCTAssertEqual(entries.dropFirst().first?.date, at(14, 15))
        XCTAssertEqual(entries.last?.date, at(23, 30))
        // Now, 14:15 to 18:15 by quarter hours, 18:30 to 23:30 by half hours.
        XCTAssertEqual(entries.count, 1 + 17 + 11)
        let quarters = entries.dropFirst().filter { $0.date <= at(18, 15) }
        let gaps = { (entries: [WatchTimelineEntry]) in zip(entries.dropFirst(), entries).map { $0.date.timeIntervalSince($1.date) } }
        XCTAssertEqual(quarters.count, 17)
        XCTAssertTrue(gaps(quarters).allSatisfy { $0 == 15 * 60 })
        let halves = entries.filter { $0.date >= at(18, 30) }
        XCTAssertTrue(gaps(halves).allSatisfy { $0 == 30 * 60 })
        XCTAssertEqual(TimelineEntries.entries(rows: [], today: today, now: at(0, 0), calendar: calendar).count, 1 + 16 + 39)
    }

    func testTheEntryCountAtTwelveToFive() {
        // Now, 17:00 to 20:45 by quarter hours, 21:00 to 23:30 by half hours.
        let entries = TimelineEntries.entries(rows: [], today: today, now: at(16, 48), calendar: calendar)
        XCTAssertEqual(entries.count, 1 + 16 + 6)
        let lastMinutes: [Int] = [1245, 1260, 1290, 1320, 1350, 1380, 1410]
        XCTAssertEqual(Array(entries.map(\.minute).suffix(7)), lastMinutes)
    }

    func testTheWindowStartsOnTheHalfHourBeforeNow() {
        let entries = TimelineEntries.entries(rows: [], today: today, now: at(14, 40), calendar: calendar)
        XCTAssertEqual(entries.map(\.windowStart).prefix(4), [870, 870, 900, 900])
        XCTAssertEqual(TimelineEntries.entries(rows: [], today: today, now: at(14, 5), calendar: calendar).first?.windowStart, 840)
    }

    func testNextSkipsDoneRowsAndCalendarRowsAlreadyStarted() {
        let rows = [
            row("Standup", .calendar, start: 840, end: 900),
            row("Brief", start: 780, end: 840, done: true),
            row("Dentist", .reminder, start: 900),
            row("Call", .task, start: 870, end: 960),
            row("Milk"),
        ]
        XCTAssertEqual(TimelineEntries.next(rows, at: 830)?.label, "Standup")
        XCTAssertEqual(TimelineEntries.next(rows, at: 845)?.label, "Call", "a started calendar event is not next")
        XCTAssertEqual(TimelineEntries.next(rows, at: 900)?.label, "Call", "a task stays next until it ends")
        XCTAssertEqual(TimelineEntries.next(rows, at: 961)?.label, nil)
        let entry = TimelineEntries.entries(rows: rows, today: today, now: at(14, 5), calendar: calendar).first
        XCTAssertEqual(entry?.left, 3)
    }

    func testTheWindowIsFourHours() {
        XCTAssertEqual(TimelineEntries.windowMinutes, 240)
    }

    func testTheSecondLineSkipsDoneRowsAndCalendarRowsAlreadyStarted() {
        let rows = [
            row("Standup", .calendar, start: 840, end: 900),
            row("Brief", start: 870, end: 900, done: true),
            row("Call", .task, start: 870, end: 960),
            row("Review", .calendar, start: 900, end: 930),
            row("Dentist", .reminder, start: 960),
            row("Milk"),
        ]
        let early = TimelineEntries.entries(rows: rows, today: today, now: at(13, 50), calendar: calendar).first
        XCTAssertEqual(early?.next?.label, "Standup")
        XCTAssertEqual(early?.following?.label, "Call", "a done row is not the second line")
        let started = TimelineEntries.entries(rows: rows, today: today, now: at(14, 50), calendar: calendar).first
        XCTAssertEqual(started?.next?.label, "Call", "a started calendar event is not next")
        XCTAssertEqual(started?.following?.label, "Review")
        let later = TimelineEntries.entries(rows: rows, today: today, now: at(15, 10), calendar: calendar).first
        XCTAssertEqual(later?.next?.label, "Call")
        XCTAssertEqual(later?.following?.label, "Dentist", "the started Review is skipped")
    }

    func testOneItemLeftHasNoSecondLine() {
        let rows = [row("Brief", start: 780, end: 810), row("Dentist", .reminder, start: 840), row("Milk")]
        let entry = TimelineEntries.entries(rows: rows, today: today, now: at(13, 50), calendar: calendar).first
        XCTAssertEqual(entry?.next?.label, "Dentist")
        XCTAssertNil(entry?.following)
    }

    func testTheHeaderNamesTheDentistFromTheMomentThePreviousItemEnds() {
        let rows = [row("Brief", start: 780, end: 810), row("Dentist", .reminder, start: 840)]
        let entries = TimelineEntries.entries(rows: rows, today: today, now: at(13, 0), calendar: calendar)
        XCTAssertEqual(entries.first { $0.date == at(13, 15) }?.next?.label, "Brief")
        XCTAssertEqual(entries.first { $0.date == at(13, 45) }?.next?.label, "Dentist")
        XCTAssertEqual(entries.first { $0.date == at(14, 0) }?.next?.label, "Dentist")
        XCTAssertNil(entries.first { $0.date == at(14, 15) }?.next)
    }

    func testAnEmptyDayAndNowAfterTheLastItem() {
        let empty = TimelineEntries.entries(rows: [], today: today, now: now, calendar: calendar)
        XCTAssertTrue(empty.allSatisfy { $0.next == nil && $0.following == nil && $0.left == 0 })
        let late = TimelineEntries.entries(rows: [row("Brief", start: 540, end: 600)], today: today, now: at(22, 10), calendar: calendar)
        XCTAssertEqual(late.count, 8)
        XCTAssertTrue(late.allSatisfy { $0.next == nil && $0.following == nil })
        XCTAssertEqual(late.first?.left, 1, "still open, though its hour has gone")
    }

    // MARK: A watch capture is a phone capture

    let indexNote = "# Reminders\n\n## Open\n\n## Done\n"

    /// The writes queued, without what is random per write: the client id
    /// and a new reminder's id.
    func queued(_ session: VaultSession) throws -> [String] {
        try session.store.pending().map { pending in
            var object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(pending.document.json().utf8)) as? [String: Any])
            object["client_id"] = nil
            let data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
            let text = try XCTUnwrap(String(data: data, encoding: .utf8))
            return text.replacingOccurrences(of: #"rem:[0-9a-f]{12}"#, with: "rem:<id>", options: .regularExpression)
        }
    }

    func testAWatchCaptureIsAPhoneCapture() throws {
        let cases: [(CaptureDestination, ReminderDue?)] = [
            (.today, nil), (.inbox, nil), (.backlog, nil),
            (.reminder, ReminderDue(date: today.adding(days: 1), time: ClockTime(hour: 9, minute: 0))),
            (.reminder, ReminderDue(date: today.adding(days: 1), time: nil)),
        ]
        let text = "Call the notary about the deed"
        for (destination, due) in cases {
            let notes = ["daily/2026-10-21.md": todayNote, "reminders.md": indexNote, "backlog.md": "# Backlog\n\n## Soon\n"]
            let phone = try vault(notes)
            let watch = try vault(notes)
            let phoneRecord = try phone.capture(blocks: Blocks.parse(text), destination: destination, due: due, now: now)
            let request = WatchCapture(at: now, text: text, destination: destination, due: due)
            XCTAssertEqual(watch.receive(.capture(request), now: now), .ack(WatchAck(id: request.id, outcome: .saved)))
            XCTAssertEqual(try queued(watch), try queued(phone), "\(destination) \(due?.description ?? "")")
            XCTAssertFalse(try queued(watch).isEmpty)
            XCTAssertEqual(watch.store.captures().map(\.digest), [try XCTUnwrap(phoneRecord?.digest)])
            XCTAssertEqual(watch.store.content("reminders.md")?.replacingOccurrences(of: #"rem:[0-9a-f]{12}"#, with: "rem:<id>", options: .regularExpression),
                           phone.store.content("reminders.md")?.replacingOccurrences(of: #"rem:[0-9a-f]{12}"#, with: "rem:<id>", options: .regularExpression))
        }
    }

    func testAReminderCaptureWithoutATimeIsRefused() throws {
        let session = try vault(["reminders.md": indexNote])
        let request = WatchCapture(at: now, text: "Something", destination: .reminder)
        XCTAssertEqual(session.receive(.capture(request), now: now), .ack(WatchAck(id: request.id, outcome: .refused, message: "That didn't save. Try again.")))
        XCTAssertTrue(session.store.pending().isEmpty)
    }

    // MARK: Exactly once

    func testTheSameCaptureHandledTwiceWritesOnce() throws {
        let session = try vault(["daily/2026-10-21.md": todayNote])
        let request = WatchMessage.capture(WatchCapture(at: now, text: "Buy stamps", destination: .inbox))
        let first = session.receive(request, now: now)
        let second = session.receive(request, now: now.addingTimeInterval(5))
        XCTAssertEqual(first, second)
        XCTAssertEqual(session.store.pending().count, 1)
        XCTAssertEqual(session.store.captures().count, 1)
    }

    func testARefusalIsAnsweredTheSameWayAgain() throws {
        let session = try vault(["daily/2026-10-21.md": todayNote])
        session.store.setMeta("status", "lapsed")
        let request = WatchMessage.capture(WatchCapture(at: now, text: "Buy stamps", destination: .inbox))
        let refused = WatchMessage.ack(WatchAck(id: try XCTUnwrap(request.requestID), outcome: .refused, message: "Thock Plus has ended. Captures are paused."))
        XCTAssertEqual(session.receive(request, now: now), refused)
        session.store.setMeta("status", nil)
        XCTAssertEqual(session.receive(request, now: now), refused, "a retry is a new request")
        XCTAssertTrue(session.store.pending().isEmpty)
    }

    func testHandledRequestsArePrunedAfterThirtyDays() throws {
        let session = try vault([:])
        let old = WatchAck(id: UUID(), outcome: .saved)
        let recent = WatchAck(id: UUID(), outcome: .saved)
        try session.store.recordWatchRequest(old, at: now.addingTimeInterval(-31 * 24 * 3600))
        try session.store.recordWatchRequest(recent, at: now.addingTimeInterval(-29 * 24 * 3600))
        _ = session.receive(.capture(WatchCapture(at: now, text: "Buy stamps", destination: .inbox)), now: now)
        XCTAssertNil(try session.store.watchRequest(id: old.id))
        XCTAssertEqual(try session.store.watchRequest(id: recent.id), recent)
    }

    // MARK: Ticks

    func testATickChangesOneLineAndIsNotAnUntick() throws {
        let session = try vault(["daily/2026-10-21.md": todayNote])
        let row = try XCTUnwrap(session.watchSnapshot(now: now).day(today)?.rows.first)
        let tick = WatchTick(day: today.iso, hash: row.hash, ordinal: row.ordinal)
        XCTAssertEqual(session.receive(.tick(tick), now: now), .ack(WatchAck(id: tick.id, outcome: .saved)))
        XCTAssertEqual(session.store.content("daily/2026-10-21.md"), todayNote.replacingOccurrences(of: "- [ ] 09:00 - 10:00 Write", with: "- [x] 09:00 - 10:00 Write"))
        let again = WatchTick(day: today.iso, hash: row.hash, ordinal: row.ordinal)
        XCTAssertEqual(session.receive(.tick(again), now: now), .ack(WatchAck(id: again.id, outcome: .saved)))
        XCTAssertEqual(session.store.pending().count, 1, "a done line stays done")
    }

    func testATickOnALineTheDeskChangedIsRefused() throws {
        let session = try vault(["daily/2026-10-21.md": todayNote])
        let row = try XCTUnwrap(session.watchSnapshot(now: now).day(today)?.rows.first)
        try session.store.applySnapshot(path: "daily/2026-10-21.md", version: 9, content: todayNote.replacingOccurrences(of: "Write the brief", with: "Write the long brief"), contentHash: "hash", blobID: "blob")
        let tick = WatchTick(day: today.iso, hash: row.hash, ordinal: row.ordinal)
        XCTAssertEqual(session.receive(.tick(tick), now: now), .ack(WatchAck(id: tick.id, outcome: .refused, message: "That line changed. Look again.")))
        XCTAssertTrue(session.store.pending().isEmpty)
    }

    func testATickOnACalendarRowIsRefused() throws {
        let session = try vault(["daily/2026-10-21.md": todayNote])
        let row = try XCTUnwrap(session.watchSnapshot(now: now).day(today)?.rows.first { $0.kind == .calendar })
        let tick = WatchTick(day: today.iso, hash: row.hash, ordinal: row.ordinal)
        XCTAssertEqual(session.receive(.tick(tick), now: now), .ack(WatchAck(id: tick.id, outcome: .refused, message: "That line changed. Look again.")))
        XCTAssertTrue(session.store.pending().isEmpty)
    }

    func testATickOnAReminderOfADayWithNoNoteClosesIt() throws {
        let session = try vault([
            "reminders.md": "## Open\n- [[projects/house]] <!--rem:7d3e55a1c0f2-->\n",
            "projects/house.md": "# House\n\n- [ ] Call the plumber (@2026-10-22 18:30) <!--rem:7d3e55a1c0f2-->\n",
        ])
        let row = try XCTUnwrap(session.watchSnapshot(now: now).day(today.adding(days: 1))?.rows.first)
        let tick = WatchTick(day: today.adding(days: 1).iso, hash: row.hash, ordinal: row.ordinal)
        XCTAssertEqual(session.receive(.tick(tick), now: now), .ack(WatchAck(id: tick.id, outcome: .saved)))
        XCTAssertEqual(session.reminderIndex(now: now).reminder(id: "7d3e55a1c0f2")?.status, .closed)
    }

    func testADayAskIsAnsweredWithThatDay() throws {
        let session = try vault(["daily/2026-12-01.md": "# Tue\n\n## Day planner\n- [ ] Far away\n"])
        let ask = WatchDayAsk(day: "2026-12-01")
        guard case .dayReply(let reply)? = session.receive(.dayAsk(ask), now: now) else { return XCTFail("no reply") }
        XCTAssertEqual(reply.id, ask.id)
        XCTAssertEqual(reply.day.rows.map(\.label), ["Far away"])
    }

    // MARK: The dictionary form

    func testEveryMessageRoundTripsThroughItsDictionary() throws {
        let session = try vault(["daily/2026-10-21.md": todayNote])
        let id = UUID()
        let at = Date(timeIntervalSince1970: 1_792_591_500)
        let messages: [WatchMessage] = [
            .snapshot(session.watchSnapshot(now: now)),
            .capture(WatchCapture(id: id, at: at, text: "Buy stamps", destination: .reminder, due: ReminderDue(date: today, time: nil))),
            .capture(WatchCapture(id: id, at: at, text: "Buy stamps", destination: .today)),
            .tick(WatchTick(id: id, day: today.iso, hash: "abc", ordinal: 1)),
            .dayAsk(WatchDayAsk(id: id, day: today.iso)),
            .ack(WatchAck(id: id, outcome: .saved)),
            .ack(WatchAck(id: id, outcome: .refused, message: "That line changed. Look again.")),
            .dayReply(WatchDayReply(id: id, day: try XCTUnwrap(session.watchSnapshot(now: now).day(today)))),
        ]
        for message in messages {
            let dictionary = try message.dictionary()
            XCTAssertTrue(PropertyListSerialization.propertyList(dictionary, isValidFor: .binary), "\(message)")
            XCTAssertEqual(dictionary["v"] as? Int, 1)
            XCTAssertEqual(try WatchMessage(dictionary: dictionary), message)
            XCTAssertEqual(try WatchMessage(data: message.encoded()), message)
        }
        let snapshot = try XCTUnwrap(messages.first?.dictionary())
        XCTAssertEqual(snapshot["today"] as? String, "2026-10-21")
        XCTAssertEqual(snapshot["state"] as? String, "ready")
        XCTAssertEqual(snapshot["made_at"] as? String, "2026-10-21T14:05:00Z")
        XCTAssertEqual((snapshot["reminders"] as? [String: Any])?["date_only_at"] as? String, "09:00")
    }

    func testAnUnknownVersionIsRejected() throws {
        var dictionary = try WatchMessage.tick(WatchTick(day: today.iso, hash: "abc", ordinal: 0)).dictionary()
        dictionary["v"] = 2
        XCTAssertThrowsError(try WatchMessage(dictionary: dictionary)) { error in
            XCTAssertEqual(error as? WatchLinkError, .unknownVersion(2))
        }
        dictionary["v"] = nil
        XCTAssertThrowsError(try WatchMessage(dictionary: dictionary)) { error in
            XCTAssertEqual(error as? WatchLinkError, .unknownVersion(nil))
        }
        XCTAssertThrowsError(try WatchMessage(dictionary: ["v": 1, "type": "fax"]))
    }

    // MARK: The watch's outbox

    func testTheOutboxKeepsARefusedCaptureAndDropsARefusedTick() {
        var outbox = WatchOutbox()
        let capture = WatchCapture(at: now, text: "Buy stamps", destination: .inbox)
        let tick = WatchTick(day: today.iso, hash: "abc", ordinal: 0)
        outbox.add(.capture(capture))
        outbox.add(.tick(tick))
        XCTAssertEqual(outbox.waitingCount, 2)
        XCTAssertTrue(outbox.isTicked(day: today.iso, hash: "abc", ordinal: 0))
        outbox.apply(WatchAck(id: tick.id, outcome: .refused, message: "That line changed. Look again."))
        XCTAssertFalse(outbox.isTicked(day: today.iso, hash: "abc", ordinal: 0))
        outbox.apply(WatchAck(id: capture.id, outcome: .refused, message: "Thock Plus has ended. Captures are paused."))
        XCTAssertEqual(outbox.unsent.map(\.state), [.refused("Thock Plus has ended. Captures are paused.")])
        let retried = outbox.retry(capture.id)
        XCTAssertNotEqual(retried?.id, capture.id, "a retry is a new request")
        XCTAssertEqual(retried?.state, .waiting)
        guard case .capture(let again)? = retried?.request else { return XCTFail("not a capture") }
        XCTAssertEqual(again.text, "Buy stamps")
        XCTAssertEqual(again.at, capture.at, "the instant it was said")
        outbox.delete(again.id)
        XCTAssertTrue(outbox.items.isEmpty)
    }

    func testASavedRequestLeavesTheListAndIsForgottenOnceASnapshotShowsIt() {
        var outbox = WatchOutbox()
        let tick = WatchTick(day: today.iso, hash: "abc", ordinal: 0)
        outbox.add(.tick(tick))
        outbox.apply(WatchAck(id: tick.id, outcome: .saved), at: now)
        XCTAssertEqual(outbox.waitingCount, 0)
        XCTAssertTrue(outbox.isTicked(day: today.iso, hash: "abc", ordinal: 0), "drawn done until the snapshot says so")
        outbox.forgetSaved(before: now.addingTimeInterval(-1))
        XCTAssertEqual(outbox.items.count, 1)
        outbox.forgetSaved(before: now.addingTimeInterval(1))
        XCTAssertTrue(outbox.items.isEmpty)
        XCTAssertNil(outbox.apply(WatchAck(id: tick.id, outcome: .saved)), "an answer to nothing changes nothing")
    }

    func testTheDraftNeverRemembersReminder() throws {
        let draft = WatchOutbox.Draft(text: "Dentist", chip: .reminder, due: ReminderDue(date: today, time: nil))
        XCTAssertEqual(draft.before, .today)
        XCTAssertEqual(draft.reminderDue, ReminderDue(date: today, time: nil))
        XCTAssertEqual(WatchOutbox.Draft(text: "x", chip: .backlog).before, .backlog)
    }

    func testTheWatchFilesSurviveARestart() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let files = WatchFiles(folder: folder)
        XCTAssertNil(files.snapshot())
        XCTAssertEqual(try files.outbox(), WatchOutbox())
        var outbox = WatchOutbox()
        outbox.add(.capture(WatchCapture(at: now, text: "Buy stamps", destination: .reminder, due: ReminderDue(date: today, time: nil))))
        outbox.items[0].transferred = true
        outbox.draft = WatchOutbox.Draft(text: "Call Ana", chip: .backlog)
        outbox.chip = .inbox
        try files.save(outbox)
        XCTAssertEqual(try WatchFiles(folder: folder).outbox(), outbox)
        let snapshot = try vault(["daily/2026-10-21.md": todayNote]).watchSnapshot(now: now)
        try files.save(snapshot)
        XCTAssertEqual(files.snapshot(), snapshot)
    }

    // MARK: Review fixes

    func testAReminderThatChangedIsRefusedAsAChangedLine() {
        XCTAssertEqual(VaultSession.refusal(for: VaultSessionError.reminderChanged), "That line changed. Look again.")
        XCTAssertEqual(VaultSession.refusal(for: VaultSessionError.lineChanged), "That line changed. Look again.")
        XCTAssertEqual(VaultSession.refusal(for: VaultSessionError.readOnly), "Thock Plus has ended. Captures are paused.")
        XCTAssertEqual(VaultSession.refusal(for: VaultSessionError.notConnected), "That didn't save. Try again.")
    }

    func testARequestTheStoreCannotLookUpIsRefusedAndNotWritten() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let url = folder.appendingPathComponent("vault.sqlite")
        let store = try VaultStore(url: url)
        store.setMeta("vault_id", "vault")
        store.setMeta("device_id", "c41a9e0d7b2f4a61")
        let session = VaultSession(store: store, calendar: calendar)
        // Another connection breaks the table under the store.
        var handle: OpaquePointer?
        XCTAssertEqual(sqlite3_open(url.path, &handle), SQLITE_OK)
        XCTAssertEqual(sqlite3_exec(handle, "DROP TABLE watch_requests", nil, nil, nil), SQLITE_OK)
        sqlite3_close(handle)
        XCTAssertThrowsError(try store.watchRequest(id: UUID()))
        let request = WatchCapture(at: now, text: "Buy stamps", destination: .inbox)
        let (reply, wrote) = session.handle(.capture(request), now: now)
        XCTAssertEqual(reply, .ack(WatchAck(id: request.id, outcome: .refused, message: "That didn't save. Try again.")))
        XCTAssertFalse(wrote)
        XCTAssertTrue(store.pending().isEmpty)
    }

    func testHandleSaysWhetherADeliveryWrote() throws {
        let session = try vault(["daily/2026-10-21.md": todayNote])
        let request = WatchMessage.capture(WatchCapture(at: now, text: "Buy stamps", destination: .inbox))
        XCTAssertTrue(session.handle(request, now: now).wrote)
        XCTAssertFalse(session.handle(request, now: now).wrote, "a repeat writes nothing")
        XCTAssertFalse(session.handle(.dayAsk(WatchDayAsk(day: today.iso)), now: now).wrote)
    }

    var newYork: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "America/New_York") ?? .gmt
        return calendar
    }

    func testEntriesFollowTheWallClockWhenTheClocksGoForward() throws {
        // Sunday 8 March 2026: 02:00 does not happen in New York.
        let calendar = newYork
        let day = VaultDay(year: 2026, month: 3, day: 8)
        let now = try XCTUnwrap(calendar.date(from: DateComponents(year: 2026, month: 3, day: 8, hour: 1, minute: 20)))
        let entries = TimelineEntries.entries(rows: [], today: day, now: now, calendar: calendar)
        XCTAssertEqual(entries.first?.minute, 80)
        XCTAssertEqual(entries.first?.windowStart, 60)
        XCTAssertFalse(entries.contains { (120..<180).contains($0.minute) }, "no entry for an hour that does not exist")
        for entry in entries {
            let parts = calendar.dateComponents([.hour, .minute], from: entry.date)
            if entry.date != now {
                XCTAssertEqual((parts.hour ?? 0) * 60 + (parts.minute ?? 0), entry.minute, "the date is the minute's wall-clock time")
            }
        }
        let afternoon = try XCTUnwrap(entries.first { $0.minute == 14 * 60 })
        XCTAssertEqual(calendar.component(.hour, from: afternoon.date), 14)
        XCTAssertEqual(entries.last?.minute, 23 * 60 + 30)
        XCTAssertEqual(entries.first { $0.minute > 5 * 60 + 20 }?.minute, 5 * 60 + 30, "half hours once the window has passed")
        XCTAssertTrue(zip(entries, entries.dropFirst()).allSatisfy { $0.date < $1.date })
    }

    func testEntriesFollowTheWallClockWhenTheClocksGoBack() throws {
        // Sunday 1 November 2026: 01:00 to 02:00 happens twice in New York.
        let calendar = newYork
        let day = VaultDay(year: 2026, month: 11, day: 1)
        let now = try XCTUnwrap(calendar.date(from: DateComponents(year: 2026, month: 11, day: 1, hour: 0, minute: 0)))
        let entries = TimelineEntries.entries(rows: [], today: day, now: now, calendar: calendar)
        XCTAssertEqual(entries.count, 1 + 16 + 39, "the repeated hour has one set of entries")
        let afternoon = try XCTUnwrap(entries.first { $0.minute == 14 * 60 })
        XCTAssertEqual(calendar.component(.hour, from: afternoon.date), 14)
        XCTAssertEqual(afternoon.windowStart, 14 * 60)
        XCTAssertTrue(zip(entries, entries.dropFirst()).allSatisfy { $0.date < $1.date })
    }

    func testReconcileLetsALostTransferGoAgain() {
        var outbox = WatchOutbox()
        let lost = WatchCapture(at: now, text: "Lost", destination: .inbox)
        let held = WatchCapture(at: now, text: "Held", destination: .inbox)
        let refused = WatchCapture(at: now, text: "Refused", destination: .inbox)
        for capture in [lost, held, refused] {
            outbox.add(.capture(capture))
        }
        for position in outbox.items.indices {
            outbox.items[position].transferred = true
        }
        outbox.apply(WatchAck(id: refused.id, outcome: .refused, message: "No"))
        XCTAssertTrue(outbox.reconcile(outstanding: [held.id]))
        XCTAssertEqual(outbox.items.map(\.transferred), [false, true, true])
        XCTAssertFalse(outbox.reconcile(outstanding: [held.id]), "nothing more to change")
    }

    func testThePhoneAndTheWatchSayTheSameWords() {
        let settings = ReminderSettings()
        let clock = ClockTime(clampingHour: 14, minute: 5)
        XCTAssertEqual(CaptureDestination.today.hint(due: nil, settings: settings, today: today, now: clock), "One line joins today's plan. More than one lands in today's note as written.")
        XCTAssertEqual(CaptureDestination.today.hint(due: nil, settings: settings, today: today, now: clock, oneParagraph: true), "One line joins today's plan.")
        XCTAssertEqual(CaptureDestination.inbox.hint(due: nil, settings: settings, today: today, now: clock), "Lands in your inbox. Triage sorts it at the desk.")
        XCTAssertEqual(CaptureDestination.backlog.hint(due: nil, settings: settings, today: today, now: clock), "Goes under Soon in your backlog.")
        let tomorrow = ReminderDue(date: today.adding(days: 1), time: ClockTime(hour: 9, minute: 0))
        XCTAssertEqual(CaptureDestination.reminder.hint(due: tomorrow, settings: settings, today: today, now: clock), "Rings tomorrow at 09:00 and waits in reminders.md.")
        XCTAssertEqual(CaptureDestination.reminder.hint(due: nil, settings: settings, today: today, now: clock), "Rings when you choose and waits in reminders.md.")
        XCTAssertEqual(CaptureDestination.reminder.hint(due: ReminderDue(date: today, time: nil), settings: settings, today: today, now: clock), "Waits in Today, in reminders.md.")
        XCTAssertEqual(CaptureDestination.allCases.map { $0.savedSentence(due: tomorrow, today: today, now: clock, dateOnlyAt: settings.dateOnlyAt) },
                       ["Added to today", "Saved to your inbox", "Added to Backlog · Soon", "Rings tomorrow at 09:00"])
        XCTAssertEqual(CaptureDestination.allCases.map(\.title), ["Today", "Inbox", "Backlog", "Reminder"])
    }

    // MARK: Saying when

    func testTheReminderChipAndSentence() {
        let due = ReminderDue(date: today.adding(days: 1), time: ClockTime(hour: 9, minute: 0))
        XCTAssertEqual(due.chipText(today: today), "⏰ Thu 09:00")
        XCTAssertEqual(due.ringsSentence(today: today, now: ClockTime(clampingHour: 14, minute: 5), dateOnlyAt: ClockTime(clampingHour: 9, minute: 0)), "Rings tomorrow at 09:00")
    }
}
