import XCTest
@testable import ThockKit

/// The inbox screen's swipes (V40 §9): each gesture's writes in order, the
/// `move_file` kind on the wire and in the store, and the receipt it leaves.
final class InboxGestureTests: XCTestCase {
    var config: VaultConfig { VaultConfig(config: SampleVault.config) }

    let inboxPath = "inbox/2026-10-10-0931-call-ana.md"
    let note = "---\nsource:   thock-ios\ncapture:  4d1f9a02c7b3\ncaptured: 2026-10-10T09:31:00Z\ntitle:    Call Ana\n---\n\n# Call Ana\n\nAbout the article on rest.\n"
    let titleOnly = "---\nsource:   thock-ios\ncapture:  9a9a9a9a9a9a\ncaptured: 2026-10-10T09:31:00Z\ntitle:    Buy milk\n---\n\n# Buy milk\n"
    let todayNote = "# 2026-10-10\n\n## Journal\n\n## Day planner\n\n- [ ] 09:00 - 09:30 Standup\n\n## Personal\n"

    func writes() -> PhoneWrites {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        // 2026-10-10 10:15 UTC
        return PhoneWrites(config: config, deviceID: "c41a9e0d7b2f4a61", now: Date(timeIntervalSince1970: 1_791_627_300), calendar: calendar)
    }

    func testAMoveFileRoundTripsAndValidates() throws {
        var write = WriteDocument(clientID: "c", kind: .moveFile, path: inboxPath, madeAt: "", deviceID: "d")
        write.toPath = "archives/inbox/2026-10-10-0931-call-ana.md"
        let json = write.json()
        XCTAssertTrue(json.contains("\"kind\":\"move_file\""), json)
        XCTAssertTrue(json.contains("\"to\":\"archives/inbox/2026-10-10-0931-call-ana.md\""), json)
        XCTAssertFalse(json.contains("heading"), json)
        let parsed = try WriteDocument.parse(json)
        XCTAssertEqual(parsed.kind, .moveFile)
        XCTAssertEqual(parsed.toPath, write.toPath)
        XCTAssertNil(parsed.to)

        XCTAssertThrowsError(try WriteDocument.parse("{\"v\":1,\"client_id\":\"c\",\"kind\":\"move_file\",\"path\":\"a.md\"}"))
        XCTAssertThrowsError(try WriteDocument.parse("{\"v\":1,\"client_id\":\"c\",\"kind\":\"move_file\",\"path\":\"a.md\",\"to\":\"a.md\"}"))
        // A move_block still reads `to` as a heading.
        let block = try WriteDocument.parse("{\"v\":1,\"client_id\":\"c\",\"kind\":\"move_block\",\"path\":\"backlog.md\",\"heading\":{\"text\":\"Soon\"},\"line_hash\":\"ab\",\"to\":{\"text\":\"Someday\"}}")
        XCTAssertEqual(block.to?.text, "Someday")
        XCTAssertNil(block.toPath)
    }

    func testApplyingAMoveChangesNoText() {
        var write = WriteDocument(clientID: "c", kind: .moveFile, path: inboxPath, madeAt: "", deviceID: "d")
        write.toPath = "archives/inbox/x.md"
        XCTAssertEqual(SyncCore.apply(existing: note, write: write), Applied(text: note, outcome: .noop))
        XCTAssertEqual(SyncCore.apply(existing: nil, write: write), Applied(text: "", outcome: .noop))
        XCTAssertFalse(SyncCore.effectPresent(content: note, write: write))
    }

    func testTheStoreRenamesTheNoteAndQueuesTheMoveOnce() throws {
        let store = try VaultStore(url: nil)
        try store.applySnapshot(path: inboxPath, version: 3, content: note, contentHash: "h3", blobID: "b3")
        var write = WriteDocument(clientID: "move-1", kind: .moveFile, path: inboxPath, madeAt: "", deviceID: "d")
        write.toPath = PhoneWrites.archivePath(for: inboxPath)

        XCTAssertEqual(try store.record([PlannedWrite(document: write)]), [.applied])
        XCTAssertNil(store.content(inboxPath))
        XCTAssertEqual(store.content("archives/inbox/2026-10-10-0931-call-ana.md"), note)
        let pending = store.pending()
        XCTAssertEqual(pending.map(\.document.kind), [.moveFile])
        XCTAssertEqual(pending.first?.document.path, inboxPath)
        XCTAssertEqual(pending.first?.baseVersion, 3)

        // Applied again, the source is gone: nothing changes, nothing queues.
        write.clientID = "move-2"
        XCTAssertEqual(try store.record([PlannedWrite(document: write)]), [.noop])
        XCTAssertEqual(store.pending().count, 1)

        // The desk's tombstone for the source, and its create for the
        // destination, leave the local copy as it is.
        try store.applyTombstone(path: inboxPath, version: 4)
        XCTAssertNil(store.content(inboxPath))
        try store.applySnapshot(path: "archives/inbox/2026-10-10-0931-call-ana.md", version: 5, content: note, contentHash: "h5", blobID: "b5")
        XCTAssertEqual(store.content("archives/inbox/2026-10-10-0931-call-ana.md"), note)
    }

    func testAMoveOntoAnExistingFileLeavesBoth() throws {
        let store = try VaultStore(url: nil)
        let archived = PhoneWrites.archivePath(for: inboxPath)
        try store.applySnapshot(path: inboxPath, version: 1, content: note, contentHash: "h1", blobID: "b1")
        try store.applySnapshot(path: archived, version: 2, content: "older", contentHash: "h2", blobID: "b2")
        var write = WriteDocument(clientID: "move-1", kind: .moveFile, path: inboxPath, madeAt: "", deviceID: "d")
        write.toPath = archived
        XCTAssertEqual(try store.record([PlannedWrite(document: write)]), [.noop])
        XCTAssertEqual(store.content(inboxPath), note)
        XCTAssertEqual(store.content(archived), "older")
        XCTAssertTrue(store.pending().isEmpty)
    }

    func testTodayIsATaskLineAMoveAndALogLine() throws {
        let planned = writes().inboxGesture(.today, path: inboxPath, note: note, todayNote: todayNote, template: nil)
        XCTAssertEqual(planned.map(\.document.kind), [.append, .moveFile, .append])

        let task = planned[0].document
        XCTAssertEqual(task.path, "daily/2026-10-10.md")
        XCTAssertEqual(task.heading, HeadingRef(text: "Day planner", level: 2))
        XCTAssertEqual(task.lines, ["- [ ] Call Ana [[2026-10-10-0931-call-ana]]"])
        XCTAssertEqual(task.placement, .beforeChildren)
        XCTAssertTrue(task.createFromTemplate)
        XCTAssertNotNil(planned[0].seed)
        let after = SyncCore.apply(existing: todayNote, write: task).text
        XCTAssertTrue(after.contains("- [ ] 09:00 - 09:30 Standup\n- [ ] Call Ana [[2026-10-10-0931-call-ana]]\n"), after)

        let move = planned[1].document
        XCTAssertEqual(move.path, inboxPath)
        XCTAssertEqual(move.toPath, "archives/inbox/2026-10-10-0931-call-ana.md")

        let log = planned[2].document
        XCTAssertEqual(log.path, VaultConfig.triageLogPath)
        XCTAssertNil(log.heading)
        XCTAssertEqual(log.lines, ["- 2026-10-10 · Call Ana → Today · Day planner <!--inbox:4d1f9a02c7b3-->"])
        // The ritual's parser reads it back, so the desk's receipts agree.
        let line = try XCTUnwrap(TriageLog.parseLine(log.lines[0]))
        XCTAssertEqual(line.digest, "4d1f9a02c7b3")
        XCTAssertEqual(line.destination, "Today · Day planner")
        XCTAssertEqual(line.day, VaultDay(year: 2026, month: 10, day: 10))
        // The log is created with no heading when missing, as the ritual
        // creates it.
        XCTAssertEqual(SyncCore.apply(existing: nil, write: log).text, log.lines[0] + "\n")
    }

    func testBacklogLandsUnderSoonAndATitleOnlyNoteCarriesNoLink() {
        let planned = writes().inboxGesture(.backlog, path: "inbox/2026-10-10-0931-buy-milk.md", note: titleOnly, todayNote: nil, template: nil)
        XCTAssertEqual(planned.map(\.document.kind), [.append, .moveFile, .append])
        let task = planned[0].document
        XCTAssertEqual(task.path, "backlog.md")
        XCTAssertEqual(task.heading, HeadingRef(text: "Soon", level: 2))
        XCTAssertEqual(task.lines, ["- [ ] Buy milk"])
        XCTAssertEqual(task.placement, .beforeChildren)
        XCTAssertFalse(task.createFromTemplate)
        XCTAssertEqual(planned[2].document.lines, ["- 2026-10-10 · Buy milk → Backlog · Soon <!--inbox:9a9a9a9a9a9a-->"])
    }

    func testArchiveIsOnlyAMoveAndALogLine() {
        let planned = writes().inboxGesture(.archive, path: inboxPath, note: note, todayNote: todayNote, template: nil)
        XCTAssertEqual(planned.map(\.document.kind), [.moveFile, .append])
        XCTAssertEqual(planned[1].document.lines, ["- 2026-10-10 · Call Ana → Archived <!--inbox:4d1f9a02c7b3-->"])
        // A hand-written note has no digest and gets a line without one.
        let bare = writes().inboxGesture(.archive, path: "inbox/thought.md", note: "# A thought\n\nmore\n", todayNote: nil, template: nil)
        XCTAssertEqual(bare[1].document.lines, ["- 2026-10-10 · A thought → Archived"])
    }

    func testABodyIsAnythingPastTheTitle() {
        XCTAssertTrue(PhoneWrites.inboxHasBody(note))
        XCTAssertFalse(PhoneWrites.inboxHasBody(titleOnly))
        XCTAssertFalse(PhoneWrites.inboxHasBody("# Just this\n\n\n"))
        XCTAssertTrue(PhoneWrites.inboxHasBody("# Title\n\n![photo](/images/2026-10-10-0931-photo.jpg)\n"))
        XCTAssertFalse(PhoneWrites.inboxHasBody("Only a line, no heading\n"))
        XCTAssertTrue(PhoneWrites.inboxHasBody("---\ntitle: x\n---\n# x\nand this\n"))
    }

    func testReceiptsReadAnArchivedLine() {
        let record = CaptureRecord(digest: "4d1f9a02c7b3", title: "Call Ana", kind: .idea, destination: .inbox, madeAt: Date(), inboxPath: inboxPath)
        let log = TriageLog.parse("- 2026-10-10 · Call Ana → Archived <!--inbox:4d1f9a02c7b3-->\n")
        XCTAssertEqual(Receipts.state(of: record, exists: { _ in false }, log: log), .archived(day: VaultDay(year: 2026, month: 10, day: 10)))
        let filed = TriageLog.parse("- 2026-10-10 · Call Ana → Today · Day planner <!--inbox:4d1f9a02c7b3-->\n")
        XCTAssertEqual(Receipts.state(of: record, exists: { _ in false }, log: filed), .filed(destination: "Today · Day planner", day: VaultDay(year: 2026, month: 10, day: 10)))
    }

    func testTheSessionWritesTheGestureEndToEnd() throws {
        let store = try VaultStore(url: nil)
        store.setMeta("vault_id", "v")
        try store.applySnapshot(path: VaultConfig.configPath, version: 1, content: SampleVault.config, contentHash: "h1", blobID: "b1")
        try store.applySnapshot(path: inboxPath, version: 2, content: note, contentHash: "h2", blobID: "b2")
        let session = VaultSession(store: store)
        let today = session.today()

        try session.triageInbox(path: inboxPath, gesture: .today)

        XCTAssertNil(store.content(inboxPath))
        XCTAssertEqual(store.content(PhoneWrites.archivePath(for: inboxPath)), note)
        let daily = try XCTUnwrap(store.content(session.config.dailyPath(today)))
        XCTAssertTrue(daily.contains("- [ ] Call Ana [[2026-10-10-0931-call-ana]]"), daily)
        let log = try XCTUnwrap(store.content(VaultConfig.triageLogPath))
        XCTAssertTrue(log.contains("· Call Ana → Today · "), log)
        XCTAssertTrue(log.contains("<!--inbox:4d1f9a02c7b3-->"), log)
        XCTAssertEqual(store.pending().map(\.document.kind), [.append, .moveFile, .append])
        XCTAssertTrue(store.waitingInboxNotes().isEmpty)

        // Gone meanwhile: nothing is written.
        try session.triageInbox(path: inboxPath, gesture: .archive)
        XCTAssertEqual(store.pending().count, 3)
    }
}
