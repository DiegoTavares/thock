import XCTest
@testable import ThockKit

final class StoreTests: XCTestCase {
    let path = "daily/2026-10-02.md"

    func append(_ line: String, clientID: String, to path: String? = nil, heading: String? = "Day planner") -> PlannedWrite {
        var write = WriteDocument(clientID: clientID, kind: .append, path: path ?? self.path, madeAt: "", deviceID: "d")
        write.heading = heading.map { HeadingRef(text: $0) }
        write.lines = [line]
        return PlannedWrite(document: write)
    }

    func snapshot(_ store: VaultStore, _ content: String, version: Int, path: String? = nil) throws {
        try store.applySnapshot(path: path ?? self.path, version: version, content: content, contentHash: "h\(version)", blobID: "b\(version)")
    }

    func testWritesAreAppliedAndQueuedInOrder() throws {
        let store = try VaultStore(url: nil)
        try snapshot(store, "## Day planner\n", version: 3)
        let outcomes = try store.record([append("- [ ] One", clientID: "c1"), append("- [ ] Two", clientID: "c2"), append("- [ ] Inbox", clientID: "c3", to: "backlog.md", heading: nil)])
        XCTAssertEqual(outcomes, [.applied, .applied, .created])
        XCTAssertEqual(store.content(path), "## Day planner\n- [ ] One\n- [ ] Two\n")
        XCTAssertEqual(store.content("backlog.md"), "- [ ] Inbox\n")
        XCTAssertEqual(store.pending().map(\.document.clientID), ["c1", "c2", "c3"])
        XCTAssertEqual(store.pending(path: path).map(\.document.clientID), ["c1", "c2"])
        XCTAssertEqual(store.pending(path: path).map(\.baseVersion), [3, 3])
        XCTAssertEqual(store.pending(path: "backlog.md").first?.baseVersion, 0)
        // A note the phone made is not the desk's yet.
        XCTAssertEqual(store.version("backlog.md"), 0)
        XCTAssertNil(store.contentHash("backlog.md"))
        XCTAssertEqual(store.version(path), 3)
    }

    func testABatchThatFailsLeavesNothingBehind() throws {
        let store = try VaultStore(url: nil)
        try snapshot(store, "## Day planner\n", version: 1)
        XCTAssertThrowsError(try store.record([append("- [ ] One", clientID: "same"), append("- [ ] Two", clientID: "same")]))
        XCTAssertEqual(store.content(path), "## Day planner\n")
        XCTAssertTrue(store.pending().isEmpty)
        try store.record([append("- [ ] Three", clientID: "next")])
        XCTAssertEqual(store.pending().map(\.document.clientID), ["next"])
    }

    func testRecordingPostsAChange() throws {
        let store = try VaultStore(url: nil)
        let posted = expectation(forNotification: VaultStore.didChange, object: store)
        try store.record([append("- [ ] One", clientID: "c1")])
        wait(for: [posted], timeout: 0)
    }

    func testDiscardRebuildsTheNoteFromTheDeskAndTheWritesStillWaiting() throws {
        let store = try VaultStore(url: nil)
        try snapshot(store, "## Day planner\n- [ ] Desk\n", version: 2)
        try store.record([append("- [ ] Refused", clientID: "c1"), append("- [ ] Kept", clientID: "c2")])
        try store.discard(clientID: "c1")
        XCTAssertEqual(store.content(path), "## Day planner\n- [ ] Desk\n- [ ] Kept\n")
        XCTAssertEqual(store.pending().map(\.document.clientID), ["c2"])
        try store.discard(clientID: "unknown")
        XCTAssertEqual(store.pending().count, 1)
    }

    func testDiscardingTheOnlyWriteOnANewNoteRemovesIt() throws {
        let store = try VaultStore(url: nil)
        try store.record([append("- [ ] New", clientID: "c1", to: "notes/new.md", heading: nil)])
        XCTAssertTrue(store.exists("notes/new.md"))
        try store.discard(clientID: "c1")
        XCTAssertFalse(store.exists("notes/new.md"))
    }

    func testRebaseReappliesPendingWritesOnEverySnapshot() throws {
        let store = try VaultStore(url: nil)
        try snapshot(store, "## Day planner\n- [ ] A\n", version: 1)
        try store.record([append("- [ ] Phone", clientID: "c1")])
        try snapshot(store, "# Day\n\n## Day planner\n- [ ] A\n- [ ] Desk\n\n## Notes\n", version: 2)
        XCTAssertEqual(store.content(path), "# Day\n\n## Day planner\n- [ ] A\n- [ ] Desk\n- [ ] Phone\n\n## Notes\n")
        XCTAssertEqual(store.version(path), 2)
        XCTAssertEqual(store.contentHash(path), "h2")
        // A snapshot that already carries the line does not get it twice.
        try snapshot(store, "## Day planner\n- [ ] Phone\n", version: 3)
        XCTAssertEqual(store.content(path), "## Day planner\n- [ ] Phone\n")
    }

    func testAReplaceLineWhoseTargetChangedAtTheDeskKeepsBoth() throws {
        let store = try VaultStore(url: nil)
        try snapshot(store, "## Day planner\n- [ ] Call Ana\n", version: 1)
        var write = WriteDocument(clientID: "c1", kind: .replaceLine, path: path, madeAt: "", deviceID: "d")
        write.heading = HeadingRef(text: "Day planner")
        write.lineHash = SyncCore.lineHash("- [ ] Call Ana")
        write.newLine = "- [x] Call Ana"
        XCTAssertEqual(try store.record([PlannedWrite(document: write)]), [.applied])
        XCTAssertEqual(store.content(path), "## Day planner\n- [x] Call Ana\n")

        try snapshot(store, "## Day planner\n- [ ] Call Ana and Bo\n", version: 2)
        XCTAssertEqual(store.content(path), "## Day planner\n- [ ] Call Ana and Bo\n- [x] Call Ana <!--thock:also-->\n")
    }

    func testATombstoneKeepsANoteAWriteIsWaitingOn() throws {
        let store = try VaultStore(url: nil)
        try snapshot(store, "## Day planner\n- [ ] Desk\n", version: 1)
        try store.record([append("- [ ] Phone", clientID: "c1")])
        try store.applyTombstone(path: path, version: 2)
        XCTAssertEqual(store.content(path), "## Day planner\n- [ ] Phone\n\n")
        XCTAssertEqual(store.version(path), 2)
    }

    func testAFullPullDropsWhatTheDeskNoLongerHasExceptPendingNotes() throws {
        let store = try VaultStore(url: nil)
        try snapshot(store, "a\n", version: 1, path: "notes/a.md")
        try snapshot(store, "b\n", version: 2, path: "notes/b.md")
        try snapshot(store, "## Day planner\n", version: 3)
        try store.record([append("- [ ] Phone", clientID: "c1")])
        try store.removeFiles(notIn: ["notes/a.md"])
        XCTAssertEqual(store.paths(), ["daily/2026-10-02.md", "notes/a.md"])
        XCTAssertEqual(store.content(path), "## Day planner\n- [ ] Phone\n\n")
        XCTAssertEqual(store.version(path), 0)
    }

    func testPruneWaitsForTheServerTheCursorAndTheSnapshot() throws {
        let store = try VaultStore(url: nil)
        try snapshot(store, "## Day planner\n", version: 4)
        try store.record([append("- [ ] One", clientID: "c1"), append("- [ ] Two", clientID: "c2")])
        store.markSent(clientID: "c1", seq: 10)
        store.markSent(clientID: "c2", seq: 11)
        XCTAssertEqual(store.pending().map(\.seq), [10, 11])

        store.cursor = 4
        try store.prune(ackedThroughSeq: 11, ackedAtVersion: 5)
        XCTAssertEqual(store.pending().count, 2, "the phone has not pulled the version the desk acked at")

        store.cursor = 5
        store.refetchPaths = [path]
        try store.prune(ackedThroughSeq: 11, ackedAtVersion: 5)
        XCTAssertEqual(store.pending().count, 2, "the snapshot carrying the writes has not arrived")

        store.refetchPaths = []
        try snapshot(store, "## Day planner\n- [ ] One\n", version: 5)
        try store.prune(ackedThroughSeq: 10, ackedAtVersion: 5)
        XCTAssertEqual(store.pending().map(\.document.clientID), ["c2"])
        // What the desk made of the pruned write is the final word.
        XCTAssertEqual(store.content(path), "## Day planner\n- [ ] One\n- [ ] Two\n")
    }

    func testPathsUnderAFolderAreAPrefixNotAPattern() throws {
        let store = try VaultStore(url: nil)
        for path in ["50%_done/a.md", "50x_done/b.md", "50%_done.md", "inbox/a.md", "inbox/sub/b.md", "inboxes/c.md", "Inbox/d.md"] {
            try snapshot(store, "x\n", version: 1, path: path)
        }
        XCTAssertEqual(store.paths(under: "50%_done"), ["50%_done/a.md"])
        XCTAssertEqual(store.paths(under: "inbox"), ["inbox/a.md", "inbox/sub/b.md"])
        XCTAssertEqual(store.paths(under: "café"), [])
    }

    func testPathsUnderAFolderWhoseNameIsUnicode() throws {
        let store = try VaultStore(url: nil)
        try snapshot(store, "x\n", version: 1, path: "café/a.md")
        XCTAssertEqual(store.paths(under: "café"), ["café/a.md"])
        try snapshot(store, "x\n", version: 1, path: "🇵🇹 trips/lisbon.md")
        XCTAssertEqual(store.paths(under: "🇵🇹 trips"), ["🇵🇹 trips/lisbon.md"])
    }

    func testMetaCursorAndRefetchPaths() throws {
        let store = try VaultStore(url: nil)
        XCTAssertEqual(store.cursor, 0)
        XCTAssertEqual(store.deviceID, "unpaired")
        XCTAssertFalse(store.isConnected)
        XCTAssertFalse(store.isReadOnly)
        store.cursor = 42
        XCTAssertEqual(store.cursor, 42)
        store.refetchPaths = ["b.md", "a.md"]
        XCTAssertEqual(store.meta("refetch"), "a.md\nb.md")
        XCTAssertEqual(store.refetchPaths, ["a.md", "b.md"])
        store.refetchPaths = []
        XCTAssertNil(store.meta("refetch"))
        store.setMeta("status", "lapsed")
        XCTAssertTrue(store.isReadOnly)
        store.setMeta("status", nil)
        XCTAssertNil(store.meta("status"))
    }

    func testCapturesAreNewestFirstAndRenameable() throws {
        let store = try VaultStore(url: nil)
        let earlier = CaptureRecord(digest: "d1", title: "First", kind: .idea, destination: .inbox, madeAt: Date(timeIntervalSince1970: 1_790_000_000.123), inboxPath: "inbox/a.md")
        let later = CaptureRecord(digest: "d2", title: "Second", kind: .link, destination: .today, madeAt: Date(timeIntervalSince1970: 1_790_000_100), inboxPath: nil)
        store.addCapture(earlier)
        store.addCapture(later)
        store.renameCapture(inboxPath: "inbox/a.md", title: "Renamed")
        let captures = store.captures()
        XCTAssertEqual(captures.map(\.title), ["Second", "Renamed"])
        XCTAssertEqual(captures.map(\.kind), [.link, .idea])
        XCTAssertEqual(captures.map(\.destination), [.today, .inbox])
        XCTAssertEqual(captures[1].madeAt.timeIntervalSince1970, 1_790_000_000.123, accuracy: 0.001)
        XCTAssertNil(captures[0].inboxPath)
    }

    func testWipeForgetsEverything() throws {
        let store = try VaultStore(url: nil)
        try snapshot(store, "## Day planner\n", version: 1)
        try store.record([append("- [ ] One", clientID: "c1")])
        store.cursor = 3
        store.addCapture(CaptureRecord(digest: "d", title: "t", kind: .idea, destination: .inbox, madeAt: Date()))
        store.wipe()
        XCTAssertTrue(store.paths().isEmpty)
        XCTAssertTrue(store.pending().isEmpty)
        XCTAssertTrue(store.captures().isEmpty)
        XCTAssertEqual(store.cursor, 0)
    }

    func testTheStoreSurvivesBeingReopened() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("thock-store-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appendingPathComponent("vault.sqlite")
        do {
            let store = try VaultStore(url: url)
            try snapshot(store, "## Day planner\n", version: 7)
            try store.record([append("- [ ] One", clientID: "c1")])
            store.markSent(clientID: "c1", seq: 2)
            store.cursor = 7
        }
        let reopened = try VaultStore(url: url)
        XCTAssertEqual(reopened.content(path), "## Day planner\n- [ ] One\n")
        XCTAssertEqual(reopened.version(path), 7)
        XCTAssertEqual(reopened.pending().map(\.seq), [2])
        XCTAssertEqual(reopened.cursor, 7)
    }
}
