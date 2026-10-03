import Foundation

/// A stand-in for the desk app, for the practice notebook and for tests. It
/// owns a vault "on disk" (a dictionary), uploads snapshots, drains the
/// phone's writes with the shared rules and acks them (V34 API §10.2, §10.4),
/// all through the same routes the real desk will call.
public actor SimulatedDesk {
    public struct Known: Codable, Equatable, Sendable {
        var version: Int
        var hash: String
    }

    public struct State: Codable, Equatable, Sendable {
        public var disk: [String: String]
        public var key: Data
        public var known: [String: Known] = [:]
        public var awake = true

        public init(disk: [String: String], key: Data) {
            self.disk = disk
            self.key = key
        }
    }

    public static let credential = "tpk_practice_desk"
    public static let backendName = "local"

    public private(set) var state: State
    private let backend: LocalBackend
    private var feedTask: Task<Void, Never>?
    private var onChange: (@Sendable (State) -> Void)?
    private let today: @Sendable () -> VaultDay

    public init(backend: LocalBackend, state: State, today: @escaping @Sendable () -> VaultDay = { VaultDay.today() }) {
        self.backend = backend
        self.state = state
        self.today = today
    }

    public func observe(_ onChange: @escaping @Sendable (State) -> Void) {
        self.onChange = onChange
    }

    public var isAwake: Bool { state.awake }

    public func file(_ path: String) -> String? {
        state.disk[path]
    }

    private func call(_ method: String, _ path: String, query: [String: String] = [:], body: [String: Any]? = nil) async throws -> [String: Any] {
        let data = try body.map { try JSONSerialization.data(withJSONObject: $0) }
        let result = try await backend.send(method: method, path: path, query: query, body: data, credential: Self.credential)
        let object = result.body.isEmpty ? [:] : (try JSONSerialization.jsonObject(with: result.body) as? [String: Any] ?? [:])
        guard (200..<300).contains(result.status) else {
            var error = (try? JSONDecoder().decode(APIError.self, from: result.body)) ?? APIError(status: result.status, code: "unavailable", error: "")
            error.status = result.status
            throw error
        }
        return object
    }

    private static func encoded(_ path: String) -> String {
        path.split(separator: "/").map { $0.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(CharacterSet(charactersIn: "/"))) ?? String($0) }.joined(separator: "/")
    }

    // MARK: Sync

    /// Creates the vault if needed, uploads what changed while "closed",
    /// drains the queue and starts listening for the phone.
    public func open() async throws {
        _ = try await call("POST", "/v1/vault", body: ["device_name": "Practice desk", "key_check": SyncCore.keyCheck(key: state.key)])
        try await catchUp()
        if state.awake {
            try await drain()
        }
        listen()
    }

    private func listen() {
        guard feedTask == nil else { return }
        let stream = backend.feed(credential: Self.credential)
        feedTask = Task { [weak self] in
            do {
                for try await event in stream {
                    guard let self else { return }
                    if case .write = event, await self.isAwake {
                        try? await self.drain()
                    }
                }
            } catch {}
        }
    }

    public func pairingLink() async throws -> PairingLink {
        let reply = try await call("POST", "/v1/vault/pairings")
        return PairingLink(code: reply["code"] as? String ?? "", key: state.key, backend: Self.backendName)
    }

    /// Asleep, the desk leaves the phone's writes queued: "waiting for the
    /// desk". Waking drains them.
    public func setAwake(_ awake: Bool) async throws {
        state.awake = awake
        onChange?(state)
        if awake {
            try await catchUp()
            try await drain()
        }
    }

    private func catchUp() async throws {
        for (path, text) in state.disk.sorted(by: { $0.key < $1.key }) where SyncCore.isSyncablePath(path) {
            if state.known[path]?.hash != SyncCore.sha256Hex(text) {
                try await upload(path)
            }
        }
        for path in state.known.keys where state.disk[path] == nil {
            let reply = try? await call("DELETE", "/v1/vault/files/" + Self.encoded(path), body: ["expected_version": state.known[path]?.version ?? 0])
            if reply != nil {
                state.known[path] = nil
            }
        }
        onChange?(state)
    }

    private func upload(_ path: String) async throws {
        guard let text = state.disk[path] else { return }
        let blobID = (0..<16).map { _ in String(format: "%02x", UInt8.random(in: 0...255)) }.joined()
        let envelope = try SyncCore.seal(key: state.key, context: .file(path: path, blobID: blobID), plaintext: Data(text.utf8))
        var expected = state.known[path]?.version ?? 0
        var begun: [String: Any]?
        for _ in 0..<2 {
            do {
                begun = try await call("POST", "/v1/vault/files/" + Self.encoded(path), body: [
                    "expected_version": expected, "blob_id": blobID, "size_bytes": envelope.count, "content_hash": SyncCore.contentHash(envelope: envelope),
                ])
                break
            } catch let error as APIError where error.code == "stale_version" {
                // The desk's record is behind; what is on disk still wins.
                expected = error.current?.version ?? expected
            }
        }
        guard let upload = (begun?["upload"] as? [String: Any])?["url"] as? String else {
            throw APIError(status: 409, code: "stale_version", error: "The upload could not begin.")
        }
        try await backend.upload(upload, body: envelope, headers: ["Content-Type": "application/octet-stream"])
        let committed = try await call("POST", "/v1/vault/files/" + Self.encoded(path) + "/commit", body: ["expected_version": expected, "blob_id": blobID])
        state.known[path] = Known(version: committed["version"] as? Int ?? 0, hash: SyncCore.sha256Hex(text))
    }

    /// Applies the queue in seq order, uploads every changed note, then acks.
    public func drain() async throws {
        var applied: [(seq: Int, path: String)] = []
        var after: Int?
        while true {
            var query: [String: String] = [:]
            if let after { query["after"] = String(after) }
            let page = try await call("GET", "/v1/vault/writes", query: query)
            let rows = page["writes"] as? [[String: Any]] ?? []
            for row in rows {
                guard let seq = row["seq"] as? Int, let clientID = row["client_id"] as? String, let path = row["path"] as? String else { continue }
                after = seq
                applied.append((seq, path))
                guard let payload = (row["payload"] as? String).flatMap({ Data(base64Encoded: $0) }),
                      let plaintext = try? SyncCore.open(key: state.key, context: .write(clientID: clientID), envelope: payload),
                      let write = try? WriteDocument.parse(String(decoding: plaintext, as: UTF8.self)),
                      write.clientID == clientID, write.path == path
                else { continue }
                let result = SyncCore.apply(existing: state.disk[path], write: write, seed: write.createFromTemplate ? seed(for: path) : nil)
                if !(state.disk[path] == nil && result.outcome == .noop) {
                    state.disk[path] = result.text
                }
            }
            if page["has_more"] as? Bool != true { break }
        }
        guard let last = applied.last?.seq else { return }
        for path in Set(applied.map(\.path)).sorted() where state.disk[path] != nil {
            if state.known[path]?.hash != SyncCore.sha256Hex(state.disk[path] ?? "") {
                try await upload(path)
            }
        }
        _ = try await call("POST", "/v1/vault/writes/ack", body: ["through_seq": last])
        onChange?(state)
    }

    private func seed(for path: String) -> String? {
        let config = VaultConfig(config: state.disk[VaultConfig.configPath], inboxConfig: state.disk[VaultConfig.inboxConfigPath])
        let base = today()
        for offset in -7...7 {
            let day = base.adding(days: offset)
            if config.dailyPath(day) == path, let template = state.disk[config.daily.template] {
                return Template.expand(template, day: day, time: "09:00", title: day.formatted(config.daily.filename))
            }
            if config.weeklyPath(day) == path, let template = state.disk[config.weekly.template] {
                return Template.expand(template, day: day, time: "09:00", title: day.formatted(config.weekly.filename))
            }
        }
        return nil
    }

    // MARK: Things that happen at the desk

    /// Any edit made at the desk: change files on disk, then upload. This
    /// happens even while "asleep", which only stops the queue from draining,
    /// so a test can put a new snapshot under writes that are still waiting.
    public func edit(_ change: @Sendable (inout [String: String]) -> Void) async throws {
        change(&state.disk)
        onChange?(state)
        try await catchUp()
    }

    /// What the Triage Inbox ritual does once the person says "all": every
    /// waiting capture becomes a Someday task, its note is removed, and the
    /// triage log gains its line (V13 §9).
    public func triageInbox() async throws -> Int {
        let config = VaultConfig(config: state.disk[VaultConfig.configPath], inboxConfig: state.disk[VaultConfig.inboxConfigPath])
        let day = today()
        let waiting = state.disk.keys.filter(config.isInboxNote).sorted()
        guard !waiting.isEmpty else { return 0 }
        var disk = state.disk
        for path in waiting {
            let note = InboxNote(path: path, content: disk[path] ?? "")
            let urgent = note.title.lowercased().contains("call") || note.title.lowercased().contains("today")
            let heading = urgent ? config.soonHeading : config.somedayHeading
            let task = note.url.map { "- [ ] [\(note.title)](\($0))" } ?? "- [ ] \(note.title)"
            var append = WriteDocument(clientID: UUID().uuidString.lowercased(), kind: .append, path: config.backlogFile, madeAt: "", deviceID: "desk")
            append.heading = HeadingRef(text: heading, level: 2)
            append.lines = [task]
            append.placement = .beforeChildren
            disk[config.backlogFile] = SyncCore.apply(existing: disk[config.backlogFile], write: append).text

            var log = disk[VaultConfig.triageLogPath] ?? "# Triage log\n\n"
            if !log.hasSuffix("\n") { log += "\n" }
            log += "- \(day.iso) · \(note.title) → Backlog · \(heading)" + (note.digest.map { " <!--inbox:\($0)-->" } ?? "") + "\n"
            disk[VaultConfig.triageLogPath] = log
            disk[path] = nil
        }
        state.disk = disk
        onChange?(state)
        try await catchUp()
        return waiting.count
    }
}
