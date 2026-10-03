import XCTest
@testable import ThockKit

/// A phone, a practice desk and the in-process server, wired the way the app
/// wires them.
final class Harness {
    let day = VaultDay(year: 2026, month: 10, day: 2)
    let backend: LocalBackend
    let desk: SimulatedDesk
    let store: VaultStore
    let secrets = MemorySecretStore()
    let engine: SyncEngine
    var transport: SyncTransport

    init(transport wrap: ((LocalBackend) -> SyncTransport)? = nil) throws {
        backend = LocalBackend()
        let day = day
        desk = SimulatedDesk(backend: backend, state: .init(disk: SampleVault.make(today: day).files, key: Data(repeating: 9, count: 32)), today: { day })
        store = try VaultStore(url: nil)
        transport = wrap?(backend) ?? backend
        engine = SyncEngine(store: store, transport: transport, secrets: secrets)
    }

    func pair() async throws {
        try await desk.open()
        try await engine.pair(link: try await desk.pairingLink(), deviceName: "Test iPhone")
    }

    var writes: PhoneWrites {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return PhoneWrites(config: store.config, deviceID: store.deviceID, now: Date(timeIntervalSince1970: 1_790_975_640), calendar: calendar)
    }

    var todayPath: String { "daily/2026-10-02.md" }

    func planner() -> Planner {
        NoteView(text: store.content(todayPath) ?? "", config: store.config).planner
    }

    /// Lets the desk's feed task run: it drains on a `write` event.
    func settle() async throws {
        for _ in 0..<50 {
            try await Task.sleep(nanoseconds: 10_000_000)
            let pending = await backend.state.writes.count
            let awake = await desk.isAwake
            if pending == 0 || !awake { break }
        }
        await engine.sync()
    }

    func assertConverged(file: StaticString = #filePath, line: UInt = #line) async {
        let disk = await desk.state.disk
        for (path, text) in disk where SyncCore.isSyncablePath(path) {
            XCTAssertEqual(store.content(path), text, path, file: file, line: line)
        }
        XCTAssertEqual(Set(store.paths()), Set(disk.keys.filter(SyncCore.isSyncablePath)), file: file, line: line)
    }
}

/// Delivers the request, then loses the answer: what a dropped connection
/// looks like to the phone.
final class LossyTransport: SyncTransport, @unchecked Sendable {
    let inner: LocalBackend
    var dropNextWriteReply = false
    /// Answers `413 too_large` to a write for this note.
    var refuseWritesTo: String?
    /// Hands back bytes that do not match the index for this note.
    var corruptDownloadsOf: String?

    init(_ inner: LocalBackend) {
        self.inner = inner
    }

    func send(method: String, path: String, query: [String: String], body: Data?, credential: String?) async throws -> HTTPResult {
        if let refused = refuseWritesTo, method == "POST", path == "/v1/vault/writes",
           let body, String(decoding: body, as: UTF8.self).contains(refused) {
            return HTTPResult(status: 413, body: Data(#"{"error":"too large","code":"too_large"}"#.utf8))
        }
        var result = try await inner.send(method: method, path: path, query: query, body: body, credential: credential)
        if let corrupt = corruptDownloadsOf, method == "GET", path == "/v1/vault/files",
           var page = try JSONSerialization.jsonObject(with: result.body) as? [String: Any],
           let files = page["files"] as? [[String: Any]] {
            page["files"] = files.map { row in
                var row = row
                if row["path"] as? String == corrupt, row["download_url"] != nil {
                    row["download_url"] = "thock-test://corrupt"
                }
                return row
            }
            result.body = try JSONSerialization.data(withJSONObject: page)
        }
        if dropNextWriteReply, method == "POST", path == "/v1/vault/writes" {
            dropNextWriteReply = false
            throw URLError(.networkConnectionLost)
        }
        return result
    }

    func download(_ url: String) async throws -> Data {
        if url == "thock-test://corrupt" {
            return Data("not the blob".utf8)
        }
        return try await inner.download(url)
    }
    func upload(_ url: String, body: Data, headers: [String: String]) async throws { try await inner.upload(url, body: body, headers: headers) }
    func feed(credential: String) -> AsyncThrowingStream<FeedEvent, Error> { inner.feed(credential: credential) }
}

final class SyncTests: XCTestCase {
    // MARK: Pairing

    func testThePairingLinkParses() throws {
        let key = Data((0..<32).map { UInt8($0) })
        let text = "thock://pair?v=1&code=K7MP-4QZX&key=\(key.base64URL)&backend=http%3A%2F%2Flocalhost%3A8080"
        let link = try XCTUnwrap(PairingLink(text))
        XCTAssertEqual(link.code, "K7MP-4QZX")
        XCTAssertEqual(link.key, key)
        XCTAssertEqual(link.backend, "http://localhost:8080")
        XCTAssertEqual(key.base64URL.count, 43)
        XCTAssertEqual(PairingLink(link.url), link)
        XCTAssertNil(PairingLink("thock://pair?v=1&code=K7MP-4QZX&key=short&backend=x"))
        XCTAssertNil(PairingLink("thock://pair?v=2&code=K7MP-4QZX&key=\(key.base64URL)&backend=x"))
        XCTAssertNil(PairingLink("https://example.com"))
    }

    func testAWrongKeyIsRefusedByItsCheck() async throws {
        let harness = try Harness()
        try await harness.desk.open()
        var link = try await harness.desk.pairingLink()
        link.key = Data(repeating: 1, count: 32)
        do {
            try await harness.engine.pair(link: link, deviceName: "Test iPhone")
            XCTFail("a mis-scanned key must not pair")
        } catch {
            XCTAssertEqual(error as? PairingError, .keyMismatch)
        }
        XCTAssertFalse(harness.store.isConnected)
        XCTAssertNil(harness.secrets.secret(SyncEngine.keyName))
    }

    func testAPairingCodeWorksOnce() async throws {
        let harness = try Harness()
        try await harness.desk.open()
        let link = try await harness.desk.pairingLink()
        try await harness.engine.pair(link: link, deviceName: "Test iPhone")
        let second = SyncEngine(store: try VaultStore(url: nil), transport: harness.backend, secrets: MemorySecretStore())
        do {
            try await second.pair(link: link, deviceName: "Another")
            XCTFail("a used code must not pair again")
        } catch {
            XCTAssertEqual(error as? PairingError, .refused("That code didn't work. Try again."))
        }
    }

    // MARK: Pull

    func testAFullPullThenIncrementalPullsMatchTheDesk() async throws {
        let harness = try Harness()
        try await harness.pair()
        await harness.assertConverged()
        XCTAssertGreaterThan(harness.store.cursor, 0)
        let state = await harness.engine.state
        XCTAssertEqual(state, .upToDate)

        try await harness.desk.edit { disk in
            disk["daily/2026-10-02.md"]? += "\n- A line added at the desk\n"
            disk["notes/new.md"] = "# New\n"
            disk["welcome.md"] = nil
            disk["photo.png"] = "not text"
        }
        await harness.engine.sync()
        await harness.assertConverged()
        XCTAssertNil(harness.store.content("welcome.md"))
        XCTAssertNil(harness.store.content("photo.png"))
        XCTAssertTrue(harness.store.content("daily/2026-10-02.md")?.hasSuffix("- A line added at the desk\n") ?? false)
    }

    func testAnUntouchedNoteIsByteIdenticalAfterAnyNumberOfRounds() async throws {
        let harness = try Harness()
        let odd = "---\r\nx: 1\r\n---\r\n\r\n| a |\r\n| - |\r\n\r\nno newline at the end"
        try await harness.desk.open()
        try await harness.desk.edit { $0["notes/odd.md"] = odd }
        try await harness.engine.pair(link: try await harness.desk.pairingLink(), deviceName: "Test iPhone")
        for _ in 0..<3 {
            await harness.engine.sync()
        }
        XCTAssertEqual(harness.store.content("notes/odd.md"), odd)
        let disk = await harness.desk.file("notes/odd.md")
        XCTAssertEqual(disk, odd)
    }

    // MARK: V34 API §10.6

    func testDeskAsleepWritesQueueThenLandInOrder() async throws {
        let harness = try Harness()
        try await harness.pair()
        try await harness.desk.setAwake(false)

        let planner = harness.planner()
        let deepWork = try XCTUnwrap(planner.items.first { $0.label.hasPrefix("Deep work") })
        try harness.store.record([try XCTUnwrap(harness.writes.tick(deepWork, planner: planner, day: harness.day))])
        try harness.store.record([try XCTUnwrap(harness.writes.addLine("Water the plants", group: nil, planner: planner, day: harness.day))])
        let captured = try XCTUnwrap(harness.writes.capture(blocks: Blocks.parse("A weekly no-plans Sunday"), destination: .inbox, todayNote: nil, template: nil, taken: harness.store.exists))
        try harness.store.record(captured.writes)
        harness.store.addCapture(captured.record)

        // The phone shows its own writes at once, and the desk has none of them.
        XCTAssertTrue(harness.store.content(harness.todayPath)?.contains("- [x] 09:30 - 11:00 Deep work") ?? false)
        await harness.engine.sync()
        XCTAssertEqual(harness.store.waitingForDeskCount, 3)
        let queued = await harness.backend.state.writes.count
        XCTAssertEqual(queued, 3)
        let deskCopy = await harness.desk.file(harness.todayPath)
        XCTAssertFalse(deskCopy?.contains("Water the plants") ?? true)
        XCTAssertEqual(harness.store.receipts().first?.state, .waiting)

        try await harness.desk.setAwake(true)
        await harness.engine.sync()
        XCTAssertEqual(harness.store.waitingForDeskCount, 0)
        await harness.assertConverged()
        let after = await harness.desk.file(harness.todayPath) ?? ""
        XCTAssertTrue(after.contains("- [x] 09:30 - 11:00 Deep work"))
        XCTAssertTrue(after.contains("- [ ] Read 20 pages 📖\n- [ ] Water the plants\n\n### Calendar"))
        let remaining = await harness.backend.state.writes.count
        XCTAssertEqual(remaining, 0)

        // Triage at the desk closes the loop on the phone.
        let filed = try await harness.desk.triageInbox()
        XCTAssertEqual(filed, 3)
        await harness.engine.sync()
        await harness.assertConverged()
        XCTAssertEqual(harness.store.receipts().first { $0.record.digest == captured.record.digest }?.state, .filed(destination: "Backlog · Someday", day: harness.day))
        XCTAssertTrue(harness.store.waitingInboxNotes().isEmpty)
    }

    func testPhoneOfflineWritesStayLocalThenPostOnceEach() async throws {
        var lossy: LossyTransport?
        let harness = try Harness { backend in
            let transport = LossyTransport(backend)
            lossy = transport
            return transport
        }
        try await harness.pair()
        await harness.backend.setOffline(true)

        let planner = harness.planner()
        let walk = try XCTUnwrap(planner.items.first { $0.label.hasPrefix("Read 20") })
        try harness.store.record([try XCTUnwrap(harness.writes.tick(walk, planner: planner, day: harness.day))])
        try harness.store.record([try XCTUnwrap(harness.writes.journalAppend(blocks: Blocks.parse("Written with no signal."), journal: NoteView(text: harness.store.content(harness.todayPath) ?? "", config: harness.store.config).journal))])
        await harness.engine.sync()
        var state = await harness.engine.state
        XCTAssertEqual(state, .offline)
        XCTAssertEqual(harness.store.pending().filter { $0.seq == nil }.count, 2)
        XCTAssertTrue(harness.store.content(harness.todayPath)?.contains("**21:14** · Written with no signal.") ?? false)

        // Back online, but the first answer is lost: the retry must not
        // leave a second copy on the server.
        await harness.backend.setOffline(false)
        try await harness.desk.setAwake(false)
        lossy?.dropNextWriteReply = true
        await harness.engine.sync()
        await harness.engine.sync()
        let server = await harness.backend.state
        XCTAssertEqual(server.writes.count, 2)
        XCTAssertEqual(server.latestSeq, 2)
        XCTAssertEqual(server.writes.map(\.clientID), harness.store.pending().map(\.document.clientID))
        XCTAssertEqual(harness.store.pending().compactMap(\.seq), [1, 2])

        try await harness.desk.setAwake(true)
        await harness.engine.sync()
        state = await harness.engine.state
        XCTAssertEqual(state, .upToDate)
        XCTAssertEqual(harness.store.waitingForDeskCount, 0)
        await harness.assertConverged()
        let after = await harness.desk.file(harness.todayPath) ?? ""
        XCTAssertEqual(after.components(separatedBy: "Written with no signal.").count, 2)
    }

    func testARefusedWriteIsDroppedAndTheQueueMovesOn() async throws {
        var lossy: LossyTransport?
        let harness = try Harness { backend in
            let transport = LossyTransport(backend)
            lossy = transport
            return transport
        }
        try await harness.pair()
        lossy?.refuseWritesTo = "\"path\":\"inbox"
        let refused = expectation(forNotification: SyncEngine.writeRefused, object: nil)

        let capture = try XCTUnwrap(harness.writes.capture(blocks: Blocks.parse("Too big to send"), destination: .inbox, todayNote: nil, template: nil, taken: { _ in false }))
        try harness.store.record(capture.writes)
        let planner = harness.planner()
        let item = try XCTUnwrap(planner.items.first { $0.label.hasPrefix("Read 20") })
        try harness.store.record([try XCTUnwrap(harness.writes.tick(item, planner: planner, day: harness.day))])
        try await harness.settle()
        // A feed-driven round may be running, which a second sync() only
        // flags; wait for the queue rather than for one call.
        for _ in 0..<100 where harness.store.waitingForDeskCount > 0 {
            try await Task.sleep(nanoseconds: 10_000_000)
            await harness.engine.sync()
        }
        await fulfillment(of: [refused], timeout: 1)

        let state = await harness.engine.state
        XCTAssertEqual(state, .upToDate)
        XCTAssertNil(harness.store.content(capture.writes[0].document.path))
        XCTAssertEqual(harness.store.waitingForDeskCount, 0)
        let desk = await harness.desk.file(harness.todayPath) ?? ""
        XCTAssertTrue(desk.contains("- [x] Read 20 pages"))
    }

    func testOneBadSnapshotDoesNotHoldTheOthersBack() async throws {
        var lossy: LossyTransport?
        let harness = try Harness { backend in
            let transport = LossyTransport(backend)
            lossy = transport
            return transport
        }
        try await harness.pair()
        let backlog = harness.store.config.backlogFile
        lossy?.corruptDownloadsOf = backlog
        try await harness.desk.edit { disk in
            disk[backlog, default: ""] += "\n- [ ] Added at the desk\n"
            disk["daily/2026-10-02.md", default: ""] += "\nA line from the desk.\n"
        }
        await harness.engine.sync()
        XCTAssertEqual(harness.store.refetchPaths, [backlog])
        XCTAssertTrue(harness.store.content(harness.todayPath)?.contains("A line from the desk.") ?? false)
        XCTAssertFalse(harness.store.content(backlog)?.contains("Added at the desk") ?? true)

        lossy?.corruptDownloadsOf = nil
        await harness.engine.sync()
        XCTAssertEqual(harness.store.refetchPaths, [])
        await harness.assertConverged()
    }

    func testTheSameLineEditedTwiceKeepsBoth() async throws {
        let harness = try Harness()
        try await harness.pair()
        try await harness.desk.setAwake(false)

        let planner = harness.planner()
        let dentist = try XCTUnwrap(planner.items.first { $0.label.hasPrefix("Call the dentist") })
        try harness.store.record([try XCTUnwrap(harness.writes.editText(dentist, text: "Call the dentist about Friday", planner: planner, day: harness.day))])
        await harness.engine.sync()

        // Meanwhile the desk rewords the same line.
        try await harness.desk.edit { disk in
            disk["daily/2026-10-02.md"] = disk["daily/2026-10-02.md"]?.replacingOccurrences(of: "Call the dentist ☎️", with: "Ring Dr. Costa ☎️")
        }
        try await harness.desk.setAwake(true)
        await harness.engine.sync()
        await harness.assertConverged()
        let text = harness.store.content(harness.todayPath) ?? ""
        XCTAssertTrue(text.contains("- [ ] 15:00 - 15:30 Ring Dr. Costa ☎️\n"))
        XCTAssertTrue(text.contains("- [ ] 15:00 - 15:30 Call the dentist about Friday <!--thock:also-->\n"))
        XCTAssertEqual(harness.store.waitingForDeskCount, 0)
        // The phone draws the kept line as any other.
        XCTAssertTrue(harness.planner().items.contains { $0.label == "Call the dentist about Friday" })
    }

    // MARK: Rebase and prune

    func testRebaseKeepsPendingWritesOnTopOfANewSnapshot() async throws {
        let harness = try Harness()
        try await harness.pair()
        try await harness.desk.setAwake(false)
        let planner = harness.planner()
        try harness.store.record([try XCTUnwrap(harness.writes.addLine("From the phone", group: nil, planner: planner, day: harness.day))])
        await harness.engine.sync()

        // The desk uploads an edit of its own before it gets to the queue.
        try await harness.desk.edit { disk in
            disk["daily/2026-10-02.md"] = disk["daily/2026-10-02.md"]?.replacingOccurrences(of: "- [ ] Read 20 pages 📖", with: "- [x] Read 20 pages 📖")
        }
        await harness.engine.sync()
        let text = harness.store.content(harness.todayPath) ?? ""
        XCTAssertTrue(text.contains("- [x] Read 20 pages 📖\n- [ ] From the phone\n"))
        XCTAssertEqual(harness.store.waitingForDeskCount, 1)

        try await harness.desk.setAwake(true)
        await harness.engine.sync()
        XCTAssertEqual(harness.store.waitingForDeskCount, 0)
        await harness.assertConverged()
    }

    func testASnapshotThatArrivesFirstDoesNotLoseTheQueue() throws {
        let store = try VaultStore(url: nil)
        try store.applySnapshot(path: "daily/2026-10-02.md", version: 5, content: "## Day planner\n- [ ] A\n", contentHash: "h1", blobID: "b1")
        var write = WriteDocument(clientID: "c1", kind: .append, path: "daily/2026-10-02.md", madeAt: "", deviceID: "d")
        write.heading = HeadingRef(text: "Day planner")
        write.lines = ["- [ ] From the phone"]
        XCTAssertEqual(try store.record([PlannedWrite(document: write)]), [.applied])
        XCTAssertEqual(store.pending().first?.baseVersion, 5)

        try store.applySnapshot(path: "daily/2026-10-02.md", version: 6, content: "## Day planner\n- [x] A\n- [ ] B\n", contentHash: "h2", blobID: "b2")
        XCTAssertEqual(store.content("daily/2026-10-02.md"), "## Day planner\n- [x] A\n- [ ] B\n- [ ] From the phone\n")
        XCTAssertEqual(store.version("daily/2026-10-02.md"), 6)

        // Not pruned before the server has it, nor before the cursor passes the ack.
        store.cursor = 6
        try store.prune(ackedThroughSeq: 10, ackedAtVersion: 6)
        XCTAssertEqual(store.pending().count, 1)
        store.markSent(clientID: "c1", seq: 3)
        try store.prune(ackedThroughSeq: 3, ackedAtVersion: 7)
        XCTAssertEqual(store.pending().count, 1)
        try store.prune(ackedThroughSeq: 2, ackedAtVersion: 6)
        XCTAssertEqual(store.pending().count, 1)

        // The desk applied it: the next snapshot carries the line, the write goes.
        try store.applySnapshot(path: "daily/2026-10-02.md", version: 7, content: "## Day planner\n- [x] A\n- [ ] B\n- [ ] From the phone\n", contentHash: "h3", blobID: "b3")
        XCTAssertEqual(store.content("daily/2026-10-02.md"), "## Day planner\n- [x] A\n- [ ] B\n- [ ] From the phone\n")
        store.cursor = 7
        try store.prune(ackedThroughSeq: 3, ackedAtVersion: 7)
        XCTAssertTrue(store.pending().isEmpty)
        XCTAssertEqual(store.content("daily/2026-10-02.md"), "## Day planner\n- [x] A\n- [ ] B\n- [ ] From the phone\n")
    }

    func testAWriteWhoseEffectIsAlreadyThereIsNotQueued() throws {
        let store = try VaultStore(url: nil)
        try store.applySnapshot(path: "backlog.md", version: 1, content: "## Soon\n- [ ] Buy milk\n", contentHash: "h", blobID: "b")
        var write = WriteDocument(clientID: "c1", kind: .append, path: "backlog.md", madeAt: "", deviceID: "d")
        write.heading = HeadingRef(text: "Soon")
        write.lines = ["- [ ] Buy milk"]
        XCTAssertEqual(try store.record([PlannedWrite(document: write)]), [.noop])
        XCTAssertTrue(store.pending().isEmpty)
    }

    func testATombstoneRemovesTheNoteUnlessAWriteIsWaitingOnIt() throws {
        let store = try VaultStore(url: nil)
        try store.applySnapshot(path: "inbox/a.md", version: 1, content: "a\n", contentHash: "h", blobID: "b")
        try store.applySnapshot(path: "daily/2026-10-02.md", version: 2, content: "## Day planner\n", contentHash: "h", blobID: "b")
        var write = WriteDocument(clientID: "c1", kind: .append, path: "daily/2026-10-02.md", madeAt: "", deviceID: "d")
        write.heading = HeadingRef(text: "Day planner")
        write.lines = ["- [ ] Kept"]
        try store.record([PlannedWrite(document: write)])
        try store.applyTombstone(path: "inbox/a.md", version: 3)
        try store.applyTombstone(path: "daily/2026-10-02.md", version: 4)
        XCTAssertNil(store.content("inbox/a.md"))
        XCTAssertEqual(store.content("daily/2026-10-02.md"), "## Day planner\n- [ ] Kept\n\n")
    }

    // MARK: Lapse, disconnect

    func testALapsedVaultGoesReadOnlyWithoutLosingTheLocalCopy() async throws {
        let harness = try Harness()
        try await harness.pair()
        let before = harness.store.content(harness.todayPath)
        await harness.backend.setLapsed(true)
        await harness.engine.sync()
        var state = await harness.engine.state
        XCTAssertEqual(state, .paused)
        XCTAssertTrue(harness.store.isReadOnly)
        XCTAssertEqual(harness.store.content(harness.todayPath), before)
        XCTAssertFalse(harness.store.paths().isEmpty)

        // The server refuses phone writes while lapsed, and says why.
        let credential = String(data: harness.secrets.secret(SyncEngine.credentialName) ?? Data(), encoding: .utf8)
        let refused = try await harness.backend.send(method: "POST", path: "/v1/vault/writes", query: [:], body: Data(#"{"client_id":"x","path":"a.md","base_version":0,"payload":""}"#.utf8), credential: credential)
        XCTAssertEqual(refused.status, 403)
        XCTAssertEqual(try JSONDecoder().decode(APIError.self, from: refused.body).code, "plus_lapsed")
        let reads = try await harness.backend.send(method: "GET", path: "/v1/vault/files", query: [:], body: nil, credential: credential)
        XCTAssertEqual(reads.status, 200)

        await harness.backend.setLapsed(false)
        await harness.engine.sync()
        state = await harness.engine.state
        XCTAssertEqual(state, .upToDate)
        XCTAssertFalse(harness.store.isReadOnly)
    }

    func testARevokedPhoneIsToldToConnectAgain() async throws {
        let harness = try Harness()
        try await harness.pair()
        let deviceID = harness.store.deviceID
        _ = try await harness.backend.send(method: "POST", path: "/v1/vault/devices/\(deviceID)/revoke", query: [:], body: nil, credential: SimulatedDesk.credential)
        await harness.engine.sync()
        let state = await harness.engine.state
        XCTAssertEqual(state, .disconnected)
        // What the phone already had stays readable.
        XCTAssertNotNil(harness.store.content(harness.todayPath))
    }

    // MARK: The local server keeps the contract

    func testTheLocalServerRefusesWhatTheContractRefuses() async throws {
        let harness = try Harness()
        try await harness.pair()
        let phone = String(data: harness.secrets.secret(SyncEngine.credentialName) ?? Data(), encoding: .utf8)
        func code(_ method: String, _ path: String, body: String? = nil, credential: String?, query: [String: String] = [:]) async throws -> (Int, String) {
            let result = try await harness.backend.send(method: method, path: path, query: query, body: body.map { Data($0.utf8) }, credential: credential)
            let code = (try? JSONDecoder().decode(APIError.self, from: result.body))?.code ?? ""
            return (result.status, code)
        }
        var answer = try await code("GET", "/v1/vault", credential: nil)
        XCTAssertEqual(answer.0, 401)
        XCTAssertEqual(answer.1, "unauthorized")
        answer = try await code("GET", "/v1/vault/writes", credential: phone)
        XCTAssertEqual(answer.1, "role_forbidden")
        answer = try await code("POST", "/v1/vault/writes", body: #"{"client_id":"x","path":"a.md","payload":"AA=="}"#, credential: SimulatedDesk.credential)
        XCTAssertEqual(answer.1, "role_forbidden")
        answer = try await code("POST", "/v1/vault/files/photo.png", body: #"{"expected_version":0,"blob_id":"00000000000000000000000000000000","size_bytes":40,"content_hash":"0000000000000000000000000000000000000000000000000000000000000000"}"#, credential: SimulatedDesk.credential)
        XCTAssertEqual(answer.0, 422)
        XCTAssertEqual(answer.1, "path_not_allowed")
        answer = try await code("POST", "/v1/vault/files/daily/2026-10-02.md", body: #"{"expected_version":1,"blob_id":"00000000000000000000000000000000","size_bytes":40,"content_hash":"0000000000000000000000000000000000000000000000000000000000000000"}"#, credential: SimulatedDesk.credential)
        XCTAssertEqual(answer.0, 409)
        XCTAssertEqual(answer.1, "stale_version")
        answer = try await code("POST", "/v1/vault/files/notes/x.md/commit", body: #"{"expected_version":0,"blob_id":"11111111111111111111111111111111"}"#, credential: SimulatedDesk.credential)
        XCTAssertEqual(answer.1, "blob_missing")
        answer = try await code("POST", "/v1/vault", body: #"{"device_name":"x","key_check":"00000000000000000000000000000000"}"#, credential: SimulatedDesk.credential)
        XCTAssertEqual(answer.1, "key_mismatch")
        answer = try await code("POST", "/v1/vault/pair", body: #"{"code":"AAAA-AAAA","device_name":"x","platform":"ios"}"#, credential: nil)
        XCTAssertEqual(answer.0, 404)
        XCTAssertEqual(answer.1, "pairing_invalid")
    }
}

final class SessionTests: XCTestCase {
    func testTheWidgetsNextLinesAndTick() async throws {
        let harness = try Harness()
        try await harness.pair()
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        let session = VaultSession(store: harness.store, calendar: calendar)
        let now = Date(timeIntervalSince1970: 1_790_975_640)

        let next = session.nextLines(limit: 3, now: now)
        XCTAssertEqual(next.map(\.label), ["Deep work: the budget spreadsheet 📊", "Lunch with Ana 🥗", "Call the dentist ☎️"])

        let first = try XCTUnwrap(next.first)
        try session.tick(hash: first.hash, ordinal: first.ordinal, day: harness.day)
        XCTAssertTrue(harness.store.content(harness.todayPath)?.contains("- [x] 09:30 - 11:00 Deep work") ?? false)
        // A calendar line is the desk's; a widget tap leaves it alone.
        let lunch = next[1]
        try session.tick(hash: lunch.hash, ordinal: lunch.ordinal, day: harness.day)
        XCTAssertTrue(harness.store.content(harness.todayPath)?.contains("- [ ] 12:30 - 13:30 Lunch with Ana") ?? false)
        XCTAssertEqual(harness.store.waitingForDeskCount, 1)
    }

    func testTheJournalContinuesAParagraphWithinTenMinutes() async throws {
        let harness = try Harness()
        try await harness.pair()
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        let session = VaultSession(store: harness.store, calendar: calendar)
        let now = Date(timeIntervalSince1970: 1_790_975_640)

        XCTAssertNil(session.journalEntryToContinue(now: now))
        XCTAssertTrue(try session.journalAppend(blocks: Blocks.parse("Walked past the bakery."), now: now))
        let entry = try XCTUnwrap(session.journalEntryToContinue(now: now.addingTimeInterval(300)))
        XCTAssertEqual(entry.time, "21:14")
        XCTAssertEqual(entry.text, "Walked past the bakery.")
        XCTAssertNil(session.journalEntryToContinue(now: now.addingTimeInterval(601)))

        try session.journalReplace(entry: entry, newText: "Walked past the bakery and thought again.", day: harness.day, now: now.addingTimeInterval(300))
        XCTAssertTrue(harness.store.content(harness.todayPath)?.contains("**21:14** · Walked past the bakery and thought again.") ?? false)
        XCTAssertEqual(session.journalEntryToContinue(now: now.addingTimeInterval(800))?.text, "Walked past the bakery and thought again.")
        XCTAssertFalse(try session.journalAppend(blocks: Blocks.parse("   "), now: now))
    }

    func testAReadOnlyPhoneWritesNothing() async throws {
        let harness = try Harness()
        try await harness.pair()
        harness.store.setMeta("status", "lapsed")
        let session = VaultSession(store: harness.store)
        XCTAssertThrowsError(try session.capture(blocks: Blocks.parse("An idea"), destination: .inbox)) { error in
            XCTAssertEqual(error as? VaultSessionError, .readOnly)
        }
        XCTAssertEqual(harness.store.waitingForDeskCount, 0)
        XCTAssertTrue(harness.store.captures().isEmpty)
    }

    func testNoteNamesForLinks() async throws {
        let harness = try Harness()
        try await harness.pair()
        let session = VaultSession(store: harness.store)
        XCTAssertEqual(session.noteTitles(matching: "wel"), ["welcome"])
        XCTAssertTrue(session.noteTitles(matching: "housel").contains("housel-on-tail-events"))
        XCTAssertFalse(session.noteTitles(matching: "").contains { $0.hasPrefix("templates/") || $0.hasPrefix(".thock") })
    }
}
