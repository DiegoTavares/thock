import XCTest
@testable import ThockKit

/// The phone's sync engine against a real Plus server (`thock/script/integration`
/// starts one). Skipped unless THOCK_INTEGRATION_URL, THOCK_INTEGRATION_ADMIN_TOKEN
/// and THOCK_INTEGRATION_INVITE are set.
struct LiveServer {
    let url: URL
    let adminToken: String
    let invite: String

    static let fromEnvironment: LiveServer? = {
        let environment = ProcessInfo.processInfo.environment
        guard let url = environment["THOCK_INTEGRATION_URL"].flatMap(URL.init(string:)),
              let adminToken = environment["THOCK_INTEGRATION_ADMIN_TOKEN"], !adminToken.isEmpty,
              let invite = environment["THOCK_INTEGRATION_INVITE"], !invite.isEmpty
        else { return nil }
        return LiveServer(url: url, adminToken: adminToken, invite: invite)
    }()

    var transport: HTTPTransport { HTTPTransport(base: url, appVersion: "integration") }

    @discardableResult
    func request(_ method: String, _ path: String, query: [String: String] = [:], body: [String: Any]? = nil, credential: String?) async throws -> Any? {
        let data = try body.map { try JSONSerialization.data(withJSONObject: $0) }
        let result = try await transport.send(method: method, path: path, query: query, body: data, credential: credential)
        guard (200..<300).contains(result.status) else {
            var error = (try? JSONDecoder().decode(APIError.self, from: result.body)) ?? APIError(status: result.status, code: "unavailable", error: String(decoding: result.body, as: UTF8.self))
            error.status = result.status
            throw error
        }
        return result.body.isEmpty ? nil : try JSONSerialization.jsonObject(with: result.body)
    }

    @discardableResult
    func admin(_ method: String, _ path: String, body: [String: Any]? = nil) async throws -> Any? {
        try await request(method, path, body: body, credential: adminToken)
    }
}

/// A minimal desk speaking the contract over HTTP: it uploads snapshots of
/// its "disk", drains the phone's writes with the shared rules and acks them
/// (V34 API §10.2, §10.4). The real desk is Rust; this is its stand-in here.
final class LiveDesk {
    let server: LiveServer
    let credential: String
    let userID: String
    let key = Data((0..<32).map { _ in UInt8.random(in: 0...255) })
    var disk: [String: String]
    private(set) var known: [String: (version: Int, hash: String)] = [:]

    /// Connects a fresh Plus user with the shared invite.
    init(server: LiveServer, disk: [String: String]) async throws {
        self.server = server
        self.disk = disk
        let device = "integration desk \(UUID().uuidString)"
        let connected = try await server.request("POST", "/v1/connect", body: ["invite_code": server.invite, "device": device], credential: nil) as? [String: Any]
        credential = try XCTUnwrap(connected?["credential"] as? String)
        let users = try await server.admin("GET", "/admin/users") as? [[String: Any]] ?? []
        userID = try XCTUnwrap(users.first { $0["device"] as? String == device }?["id"] as? String)
    }

    @discardableResult
    func call(_ method: String, _ path: String, query: [String: String] = [:], body: [String: Any]? = nil) async throws -> [String: Any] {
        try await server.request(method, path, query: query, body: body, credential: credential) as? [String: Any] ?? [:]
    }

    static func encoded(_ path: String) -> String {
        path.split(separator: "/", omittingEmptySubsequences: false)
            .map { $0.addingPercentEncoding(withAllowedCharacters: CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")) ?? String($0) }
            .joined(separator: "/")
    }

    func open() async throws {
        try await call("POST", "/v1/vault", body: ["device_name": "Integration desk", "key_check": SyncCore.keyCheck(key: key)])
    }

    func pairingLink() async throws -> PairingLink {
        let reply = try await call("POST", "/v1/vault/pairings")
        return PairingLink(code: try XCTUnwrap(reply["code"] as? String), key: key, backend: server.url.absoluteString)
    }

    var vault: [String: Any] {
        get async throws { try await call("GET", "/v1/vault") }
    }

    /// Uploads every syncable note that differs from what the server holds and
    /// tombstones the ones gone from disk.
    func push() async throws {
        for (path, text) in disk.sorted(by: { $0.key < $1.key }) where SyncCore.isSyncablePath(path) {
            if known[path]?.hash != SyncCore.sha256Hex(text) {
                try await upload(path, text)
            }
        }
        for (path, record) in known where disk[path] == nil {
            try await call("DELETE", "/v1/vault/files/" + Self.encoded(path), body: ["expected_version": record.version])
            known[path] = nil
        }
    }

    private func upload(_ path: String, _ text: String) async throws {
        let blobID = (0..<16).map { _ in String(format: "%02x", UInt8.random(in: 0...255)) }.joined()
        let envelope = try SyncCore.seal(key: key, context: .file(path: path, blobID: blobID), plaintext: Data(text.utf8))
        let expected = known[path]?.version ?? 0
        let begun = try await call("POST", "/v1/vault/files/" + Self.encoded(path), body: [
            "expected_version": expected, "blob_id": blobID, "size_bytes": envelope.count, "content_hash": SyncCore.contentHash(envelope: envelope),
        ])
        let upload = try XCTUnwrap(begun["upload"] as? [String: Any])
        try await server.transport.upload(try XCTUnwrap(upload["url"] as? String), body: envelope, headers: upload["headers"] as? [String: String] ?? [:])
        let committed = try await call("POST", "/v1/vault/files/" + Self.encoded(path) + "/commit", body: ["expected_version": expected, "blob_id": blobID])
        known[path] = (try XCTUnwrap(committed["version"] as? Int), SyncCore.sha256Hex(text))
    }

    /// Applies the queue in seq order, uploads the notes it changed, acks.
    /// Returns how many writes it applied.
    @discardableResult
    func drain() async throws -> Int {
        let page = try await call("GET", "/v1/vault/writes")
        let rows = page["writes"] as? [[String: Any]] ?? []
        guard page["has_more"] as? Bool != true else {
            throw XCTSkip("more writes than one page; this desk does not page")
        }
        var last: Int?
        for row in rows {
            let clientID = try XCTUnwrap(row["client_id"] as? String)
            let path = try XCTUnwrap(row["path"] as? String)
            let payload = try XCTUnwrap((row["payload"] as? String).flatMap { Data(base64Encoded: $0) })
            let plaintext = try SyncCore.open(key: key, context: .write(clientID: clientID), envelope: payload)
            let write = try WriteDocument.parse(String(decoding: plaintext, as: UTF8.self))
            XCTAssertEqual(write.path, path)
            disk[path] = SyncCore.apply(existing: disk[path], write: write).text
            last = try XCTUnwrap(row["seq"] as? Int)
        }
        guard let last else { return 0 }
        try await push()
        try await call("POST", "/v1/vault/writes/ack", body: ["through_seq": last])
        return rows.count
    }
}

final class LiveServerTests: XCTestCase {
    let day = VaultDay(year: 2026, month: 10, day: 2)
    var todayPath: String { "daily/2026-10-02.md" }
    var server: LiveServer!

    override func setUp() async throws {
        guard let live = LiveServer.fromEnvironment else {
            throw XCTSkip("THOCK_INTEGRATION_URL is not set; run thock/script/integration")
        }
        server = live
    }

    private func phone() throws -> (store: VaultStore, engine: SyncEngine) {
        let store = try VaultStore(url: nil)
        return (store, SyncEngine(store: store, transport: server.transport, secrets: MemorySecretStore()))
    }

    private func writes(_ store: VaultStore) -> PhoneWrites {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return PhoneWrites(config: store.config, deviceID: store.deviceID, now: Date(timeIntervalSince1970: 1_790_975_640), calendar: calendar)
    }

    private func planner(_ store: VaultStore) -> Planner {
        NoteView(text: store.content(todayPath) ?? "", config: store.config).planner
    }

    private func pairedDesk(extra: [String: String] = [:]) async throws -> (LiveDesk, VaultStore, SyncEngine) {
        let desk = try await LiveDesk(server: server, disk: SampleVault.make(today: day).files.merging(extra) { $1 })
        try await desk.open()
        try await desk.push()
        let (store, engine) = try phone()
        try await engine.pair(link: try await desk.pairingLink(), deviceName: "Integration iPhone")
        return (desk, store, engine)
    }

    private func assertConverged(_ desk: LiveDesk, _ store: VaultStore, file: StaticString = #filePath, line: UInt = #line) {
        let syncable = desk.disk.filter { SyncCore.isSyncablePath($0.key) }
        for (path, text) in syncable {
            XCTAssertEqual(store.content(path), text, path, file: file, line: line)
        }
        XCTAssertEqual(Set(store.paths()), Set(syncable.keys), file: file, line: line)
    }

    // MARK: The round trip

    func testDeskAndPhoneConvergeThroughTheServer() async throws {
        // Paths that need percent-encoding are where the three encoders can
        // disagree.
        let odd = ["notes/Café & ideas #1.md": "# Café\n\nwith ünïcode — and a tab\there\n", "notes/100% done?.md": "no newline at the end"]
        let (desk, store, engine) = try await pairedDesk(extra: odd)
        var state = await engine.state
        XCTAssertEqual(state, .upToDate)
        XCTAssertGreaterThan(store.cursor, 0)
        assertConverged(desk, store)

        // The phone ticks a task, adds one and captures to the inbox.
        let planner = planner(store)
        let deepWork = try XCTUnwrap(planner.items.first { $0.label.hasPrefix("Deep work") })
        try store.record([try XCTUnwrap(writes(store).tick(deepWork, note: .day(day)))])
        try store.record([try XCTUnwrap(writes(store).addLine("Water the plants", group: nil, planner: planner, note: .day(day)))])
        let captured = try XCTUnwrap(writes(store).capture(blocks: Blocks.parse("A weekly no-plans Sunday"), destination: .inbox, todayNote: nil, template: nil, taken: store.exists))
        try store.record(captured.writes)
        store.addCapture(captured.record)
        await engine.sync()
        XCTAssertEqual(store.waitingForDeskCount, 3)
        let pending = try await desk.vault["writes"] as? [String: Any]
        XCTAssertEqual(pending?["pending"] as? Int, 3)

        let applied = try await desk.drain()
        XCTAssertEqual(applied, 3)
        XCTAssertTrue(desk.disk[todayPath]?.contains("- [x] 09:30 - 11:00 Deep work") ?? false)
        XCTAssertTrue(desk.disk[todayPath]?.contains("- [ ] Water the plants") ?? false)
        let inboxPath = try XCTUnwrap(captured.record.inboxPath)
        XCTAssertNotNil(desk.disk[inboxPath])

        await engine.sync()
        state = await engine.state
        XCTAssertEqual(state, .upToDate)
        XCTAssertEqual(store.waitingForDeskCount, 0)
        assertConverged(desk, store)

        // Desk edits and a deletion reach the phone as an incremental pull.
        desk.disk[todayPath, default: ""] += "\nA line from the desk.\n"
        desk.disk["notes/100% done?.md"] = nil
        try await desk.push()
        await engine.sync()
        assertConverged(desk, store)
        XCTAssertNil(store.content("notes/100% done?.md"))
    }

    func testTheFeedNudgesThePhone() async throws {
        let (desk, store, engine) = try await pairedDesk()
        await engine.start()
        defer { Task { await engine.stop() } }
        // Give the feed a moment to open before the desk changes anything.
        try await Task.sleep(nanoseconds: 300_000_000)
        desk.disk["notes/from the feed.md"] = "# Arrived without a poll\n"
        try await desk.push()
        for _ in 0..<100 where store.content("notes/from the feed.md") == nil {
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        XCTAssertEqual(store.content("notes/from the feed.md"), "# Arrived without a poll\n")
    }

    /// The phone writes against a version the desk has since replaced: the
    /// desk applies it to the newer text and nothing is lost on either side.
    func testAWriteOnAStaleBaseLandsOnTheNewerText() async throws {
        let (desk, store, engine) = try await pairedDesk()
        let baseVersion = store.version(todayPath)
        desk.disk[todayPath, default: ""] += "\nWritten at the desk meanwhile.\n"
        try await desk.push()

        let planner = planner(store)
        let item = try XCTUnwrap(planner.items.first { $0.label.hasPrefix("Read 20") })
        try store.record([try XCTUnwrap(writes(store).tick(item, note: .day(day)))])
        await engine.sync()
        XCTAssertEqual(store.pending().first?.baseVersion, baseVersion)

        try await desk.drain()
        await engine.sync()
        assertConverged(desk, store)
        let text = store.content(todayPath) ?? ""
        XCTAssertTrue(text.contains("- [x] Read 20 pages"))
        XCTAssertTrue(text.contains("Written at the desk meanwhile."))
        XCTAssertEqual(store.waitingForDeskCount, 0)
    }

    // MARK: Credentials that stop working

    func testARevokedPhoneIsDisconnected() async throws {
        let (desk, store, engine) = try await pairedDesk()
        let devices = try await desk.vault["devices"] as? [[String: Any]] ?? []
        let phoneID = try XCTUnwrap(devices.first { $0["role"] as? String == "phone" }?["device_id"] as? String)
        XCTAssertEqual(phoneID, store.deviceID)
        try await desk.call("POST", "/v1/vault/devices/\(phoneID)/revoke")

        let planner = planner(store)
        let item = try XCTUnwrap(planner.items.first { $0.label.hasPrefix("Read 20") })
        try store.record([try XCTUnwrap(writes(store).tick(item, note: .day(day)))])
        await engine.sync()
        let state = await engine.state
        XCTAssertEqual(state, .disconnected)
        XCTAssertEqual(store.pending().filter { $0.seq == nil }.count, 1, "nothing reached a revoked credential")
        let queued = try await desk.vault["writes"] as? [String: Any]
        XCTAssertEqual(queued?["pending"] as? Int, 0)
    }

    func testALapsedVaultIsReadOnlyThenResumes() async throws {
        let (desk, store, engine) = try await pairedDesk()
        try await server.admin("POST", "/admin/users/\(desk.userID)/vault/lapse", body: ["lapsed": true])

        await engine.sync()
        var state = await engine.state
        XCTAssertEqual(state, .paused)
        XCTAssertTrue(store.isReadOnly)
        do {
            desk.disk[todayPath, default: ""] += "\nNot while lapsed.\n"
            try await desk.push()
            XCTFail("a lapsed desk must not upload")
        } catch let error as APIError {
            XCTAssertEqual(error.code, "plus_lapsed")
        }

        try await server.admin("POST", "/admin/users/\(desk.userID)/vault/lapse", body: ["lapsed": false])
        try await desk.push()
        await engine.sync()
        state = await engine.state
        XCTAssertEqual(state, .upToDate)
        XCTAssertFalse(store.isReadOnly)
        assertConverged(desk, store)
    }

    func testAPairingCodeWorksOnceAgainstTheServer() async throws {
        let desk = try await LiveDesk(server: server, disk: SampleVault.make(today: day).files)
        try await desk.open()
        let link = try await desk.pairingLink()
        let (_, first) = try phone()
        try await first.pair(link: link, deviceName: "First")
        let (secondStore, second) = try phone()
        do {
            try await second.pair(link: link, deviceName: "Second")
            XCTFail("a used code must not pair again")
        } catch let error as PairingError {
            guard case .refused = error else { return XCTFail("unexpected \(error)") }
        }
        XCTAssertFalse(secondStore.isConnected)
    }
}
