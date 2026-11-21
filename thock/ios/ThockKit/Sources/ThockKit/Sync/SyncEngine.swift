import Foundation

public enum SyncState: Equatable, Sendable {
    case notConnected
    case upToDate
    case working
    /// The network is away; writes keep landing locally and wait.
    case offline
    /// Plus lapsed: read-only on what the phone already has.
    case paused
    /// The desk disconnected this phone, or rotated the key.
    case disconnected
}

public enum PairingError: Error, Equatable {
    case badLink
    /// The scanned key does not open this vault: a scan error.
    case keyMismatch
    case refused(String)
}

/// What the last rounds saw, for the settings sheet to show when something
/// is not arriving. Plain facts; the sentences are the UI's.
public struct SyncDiagnostics: Equatable, Sendable {
    public var lastRound: Date?
    public var lastError: String?
    public var serverFileCount: Int?
    public var serverLatestVersion: Int?
    public var serverPendingWrites: Int?
    public var cursor = 0
    /// Path and reason for every snapshot that could not be taken in the
    /// last pull.
    public var failures: [String] = []
    /// Why the service last refused this phone's push token, until it takes
    /// one; while it is set, desk changes wait for the app to open.
    public var pushTokenError: String?

    public init() {}
}

/// The phone's whole sync logic (V34 §11): send queued writes, pull
/// snapshots, rebase, prune. Screens read the local store and never wait on
/// any of this.
public actor SyncEngine {
    public static let stateDidChange = Notification.Name("ThockSyncStateDidChange")
    /// Posted with the note's path as `object` when a write was refused for
    /// good and dropped.
    public static let writeRefused = Notification.Name("ThockSyncWriteRefused")
    /// Refusals that no retry can change: the write is dropped so the ones
    /// behind it still go (V34 API §3).
    static let permanentRefusals: Set<String> = ["too_large", "path_not_allowed", "bad_request"]
    static let keyName = "vault-key"
    static let credentialName = "device-credential"

    private let store: VaultStore
    private let transport: SyncTransport
    private let secrets: SecretStore
    private var feedTask: Task<Void, Never>?
    /// The rounds in flight, so an engine that is set aside can end them.
    private var roundsTask: Task<Void, Never>?
    private var running = false
    private var again = false
    /// Callers that asked for a round while one was running, resumed when
    /// the loop that took their request ends.
    private var waiting: [CheckedContinuation<Void, Never>] = []
    /// The push token the service last took from this instance. Kept in
    /// memory only, so every launch offers it once: the service may have
    /// forgotten a token the phone still holds (V50 F1).
    private var acceptedPushToken: String?

    public private(set) var state: SyncState
    public private(set) var diagnostics = SyncDiagnostics()

    public init(store: VaultStore, transport: SyncTransport, secrets: SecretStore) {
        self.store = store
        self.transport = transport
        self.secrets = secrets
        if !store.isConnected {
            state = .notConnected
        } else if store.isReadOnly {
            state = .paused
        } else {
            state = .upToDate
        }
    }

    private func set(_ newState: SyncState) {
        guard newState != state else { return }
        state = newState
        NotificationCenter.default.post(name: Self.stateDidChange, object: nil)
    }

    private var credential: String? {
        secrets.secret(Self.credentialName).flatMap { String(data: $0, encoding: .utf8) }
    }

    private func call<T: Decodable>(_ method: String, _ path: String, query: [String: String] = [:], body: [String: Any?]? = nil, credential: String?, as type: T.Type) async throws -> T {
        let data = try body.map { try JSONSerialization.data(withJSONObject: $0.mapValues { $0 ?? NSNull() }) }
        let result = try await transport.send(method: method, path: path, query: query, body: data, credential: credential)
        guard (200..<300).contains(result.status) else {
            if var error = try? JSONDecoder().decode(APIError.self, from: result.body) {
                error.status = result.status
                throw error
            }
            throw APIError(status: result.status, code: "unavailable", error: "Something went wrong. Try again in a moment.")
        }
        return try JSONDecoder().decode(type, from: result.body)
    }

    /// Asks the service what the agent on this phone may spend. Asking is
    /// also what keeps the allowance current while the desk is closed.
    public func agentGrant() async throws -> AgentGrant {
        guard let credential else {
            throw APIError(status: 401, code: "unauthorized", error: "This phone isn't connected to your desk.")
        }
        return try await call("GET", "/v1/vault/agent", credential: credential, as: AgentGrant.self)
    }

    // MARK: Pairing

    /// Redeems the code from the desk's QR, checks the scanned key against
    /// the vault's before keeping anything, then pulls the whole vault.
    public func pair(link: PairingLink, deviceName: String, apnsToken: String? = nil) async throws {
        let response: PairResponse
        do {
            response = try await call("POST", "/v1/vault/pair", body: [
                "code": link.code,
                "device_name": deviceName,
                "platform": "ios",
                "apns_token": apnsToken,
            ], credential: nil, as: PairResponse.self)
        } catch let error as APIError {
            throw PairingError.refused(error.error)
        }
        guard response.vault.keyCheck == SyncCore.keyCheck(key: link.key) else {
            throw PairingError.keyMismatch
        }
        store.wipe()
        secrets.setSecret(Self.keyName, link.key)
        secrets.setSecret(Self.credentialName, Data(response.credential.utf8))
        // The Keychain can refuse (no passcode yet, a full disk). Nothing is
        // recorded as connected unless both secrets read back.
        guard secrets.secret(Self.keyName) == link.key, secrets.secret(Self.credentialName) == Data(response.credential.utf8) else {
            secrets.setSecret(Self.keyName, nil)
            secrets.setSecret(Self.credentialName, nil)
            throw PairingError.refused("This phone couldn't keep the key in its Keychain. Try connecting again.")
        }
        store.setMeta("vault_id", response.vault.vaultID)
        store.setMeta("device_id", response.device.deviceID)
        store.setMeta("device_name", response.device.name ?? deviceName)
        store.setMeta("backend", link.backend)
        store.setMeta("status", response.vault.status)
        acceptedPushToken = apnsToken
        set(.working)
        await sync()
    }

    /// Sends a problem report to the service, which files it for a person
    /// to read (V41). Returns the report's number.
    public func sendReport(_ report: IssueReport, description: String, screenshots: [Data]) async throws -> Int {
        guard let credential else {
            throw APIError(status: 401, code: "unauthorized", error: "This phone isn't connected to your desk.")
        }
        struct Filed: Decodable { var number: Int }
        let filed = try await call("POST", "/v1/vault/feedback", body: report.payload(description: description, screenshots: screenshots), credential: credential, as: Filed.self)
        return filed.number
    }

    /// Forgets the vault on this phone. The desk's copy is untouched.
    public func disconnect() {
        stop(endingRound: true)
        secrets.setSecret(Self.keyName, nil)
        secrets.setSecret(Self.credentialName, nil)
        store.wipe()
        set(.notConnected)
    }

    /// Tells the service where to send the silent push that wakes the phone
    /// to sync (V34 API §6.5). Offered once per instance and again for a
    /// different token; a refused offer is retried on the next call.
    public func updatePushToken(_ token: String) async {
        guard let credential, acceptedPushToken != token else { return }
        do {
            _ = try await call("PATCH", "/v1/vault/devices/me", body: ["apns_token": token], credential: credential, as: DeviceInfo.self)
            acceptedPushToken = token
            diagnostics.pushTokenError = nil
        } catch {
            diagnostics.pushTokenError = (error as? APIError)?.error ?? error.localizedDescription
        }
    }

    // MARK: The loop

    /// Holds the feed while the app is in the foreground; the app calls
    /// `stop()` on the way out, because iOS suspends it without closing the
    /// stream and an open stream keeps the server's instance billed. Events
    /// are nudges: every connect and reconnect starts with a full round
    /// regardless, and a start while the feed is already held runs one.
    public func start() {
        guard store.isConnected else { return }
        guard feedTask == nil else {
            Task { await sync() }
            return
        }
        feedTask = Task { [weak self] in
            var delay: UInt64 = 1
            while !Task.isCancelled {
                guard let self, let credential = await self.credential else { return }
                await self.sync()
                if await self.state == .disconnected { return }
                do {
                    for try await event in self.transport.feed(credential: credential) {
                        delay = 1
                        await self.handle(event)
                    }
                } catch {
                    if Task.isCancelled { return }
                }
                try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
                delay = min(delay * 2, 60)
            }
        }
    }

    /// Lets go of the feed. A round in flight finishes on its own, so a note
    /// written as the app leaves is still sent. `endingRound` cancels it
    /// too, for an engine that is being replaced or forgotten: its round
    /// would go on writing to a store that is no longer its own.
    public func stop(endingRound: Bool = false) {
        feedTask?.cancel()
        feedTask = nil
        if endingRound {
            roundsTask?.cancel()
        }
    }

    private func handle(_ event: FeedEvent) async {
        switch event {
        case .file, .ack:
            await sync()
        case .vault(let status):
            if status == "deleted" {
                set(.disconnected)
            } else {
                store.setMeta("status", status)
                await sync()
            }
        case .device(let deviceID, let revoked):
            if revoked, deviceID == store.deviceID {
                set(.disconnected)
            }
        case .write:
            break
        }
    }

    /// One round: send what is queued, pull what changed, drop what the desk
    /// has applied. Safe to call from anywhere, any number of times. Called
    /// while a round runs, it returns once a round that started after the
    /// call has finished, so a background caller waits for the data.
    public func sync() async {
        guard store.isConnected else { return }
        if running {
            again = true
            await withCheckedContinuation { waiting.append($0) }
            return
        }
        running = true
        // A task of the engine's own, not the caller's: the feed loop is
        // cancelled when the app leaves, and rounds cancelled with it would
        // fail every request, the send of a note written on the way out
        // included.
        let task = Task { await self.rounds() }
        roundsTask = task
        await task.value
        roundsTask = nil
        running = false
        let finished = waiting
        waiting = []
        for continuation in finished {
            continuation.resume()
        }
    }

    private func rounds() async {
        repeat {
            again = false
            await round()
        } while again && !Task.isCancelled
    }

    private func round() async {
        guard let credential, let key = secrets.secret(Self.keyName) else {
            set(.disconnected)
            return
        }
        set(store.isReadOnly ? .paused : .working)
        diagnostics.lastRound = Date()
        diagnostics.failures = []
        do {
            // A write that can't be sent yet must not keep the desk's
            // changes from arriving: the pull runs either way, and the
            // round still ends as failed so the send is tried again.
            var unsent: Error?
            if !store.isReadOnly {
                do {
                    try await flush(credential: credential, key: key)
                } catch {
                    unsent = error
                }
            }
            try await pull(credential: credential, key: key)
            let vault = try await call("GET", "/v1/vault", credential: credential, as: VaultInfo.self)
            store.setMeta("status", vault.status)
            if let writes = vault.writes {
                try store.prune(ackedThroughSeq: writes.ackedThroughSeq ?? 0, ackedAtVersion: writes.ackedAtVersion ?? 0)
            }
            diagnostics.serverFileCount = vault.fileCount
            diagnostics.serverLatestVersion = vault.latestVersion
            diagnostics.serverPendingWrites = vault.writes?.pending
            diagnostics.cursor = store.cursor
            if let unsent {
                throw unsent
            }
            diagnostics.lastError = nil
            set(vault.status == "lapsed" ? .paused : .upToDate)
        } catch let error as APIError {
            diagnostics.lastError = "\(error.status) \(error.code): \(error.error)"
            switch error.code {
            case "unauthorized", "vault_missing":
                set(.disconnected)
            case "plus_lapsed":
                store.setMeta("status", "lapsed")
                set(.paused)
            case "revoked", "plan_excludes_sync":
                set(.paused)
            default:
                set(.offline)
            }
        } catch {
            diagnostics.lastError = error.localizedDescription
            set(.offline)
        }
        NotificationCenter.default.post(name: Self.stateDidChange, object: nil)
    }

    /// Posts unsent writes in the order they were made. A retry reuses the
    /// write's `client_id`, so the server never holds a duplicate.
    private func flush(credential: String, key: Data) async throws {
        for write in store.pending() where write.seq == nil {
            let json = write.document.json()
            let envelope = try SyncCore.seal(key: key, context: .write(clientID: write.document.clientID), plaintext: Data(json.utf8))
            let accepted: WriteAccepted
            do {
                accepted = try await call("POST", "/v1/vault/writes", body: [
                    "client_id": write.document.clientID,
                    "path": write.document.path,
                    "base_version": write.baseVersion,
                    "payload": envelope.base64EncodedString(),
                ], credential: credential, as: WriteAccepted.self)
            } catch let error as APIError where Self.permanentRefusals.contains(error.code) {
                diagnostics.failures.append("\(write.document.path): refused, \(error.code)")
                try store.discard(clientID: write.document.clientID)
                NotificationCenter.default.post(name: Self.writeRefused, object: write.document.path)
                continue
            }
            store.markSent(clientID: write.document.clientID, seq: accepted.seq)
        }
    }

    private func pull(credential: String, key: Data) async throws {
        // No cursor means a full pull: every live note, after which anything
        // the desk no longer has is removed here too. A snapshot that didn't
        // arrive last round is asked for again by starting just below it,
        // which lists what changed since and not the whole vault.
        let cursor = store.cursor
        var since: Int? = cursor > 0 ? min(cursor, store.refetchSince ?? cursor) : nil
        var fullPull = since == nil
        var listed: Set<String> = []
        var failed: Set<String> = []
        var retryFrom: Int?
        // A note owed from an earlier round that this listing never reaches
        // (it arrived and couldn't be read, so it isn't asked for again) is
        // still owed: its writes are kept until a version that reads lands.
        let owed = store.refetchPaths
        var seen: Set<String> = []
        while true {
            var query = ["limit": "500"]
            if let since {
                query["since"] = String(since)
                // Later pages of a full pull continue from its last row, which
                // the server lets outlive pruned tombstones.
                if fullPull {
                    query["full"] = "1"
                }
            }
            let page: FilesPage
            do {
                page = try await call("GET", "/v1/vault/files", query: query, credential: credential, as: FilesPage.self)
            } catch let error as APIError where error.code == "cursor_expired" && !fullPull {
                since = nil
                fullPull = true
                listed = []
                failed = []
                retryFrom = nil
                seen = []
                continue
            }
            for row in page.files {
                // A failed download is only noted, so without this a round
                // ended with its engine would run on and record the pull.
                try Task.checkCancellation()
                seen.insert(row.path)
                let taken: Taken
                if row.deleted == true {
                    do {
                        try store.applyTombstone(path: row.path, version: row.version)
                        taken = .stored
                    } catch {
                        taken = .retry("could not be removed, \(error.localizedDescription)")
                    }
                } else {
                    listed.insert(row.path)
                    taken = await take(row, key: key)
                }
                switch taken {
                case .stored:
                    break
                case .retry(let why):
                    diagnostics.failures.append("\(row.path): \(why)")
                    failed.insert(row.path)
                    retryFrom = min(retryFrom ?? row.version - 1, row.version - 1)
                case .unreadable(let why):
                    diagnostics.failures.append("\(row.path): \(why)")
                    failed.insert(row.path)
                }
            }
            since = page.nextSince ?? page.files.map(\.version).max() ?? since ?? 0
            if page.hasMore != true { break }
        }
        try Task.checkCancellation()
        // A path whose snapshot failed was still listed, so it survives this;
        // desk deletions keep arriving while it is fetched again.
        if fullPull {
            try store.removeFiles(notIn: listed)
        }
        store.cursor = fullPull ? since ?? 0 : max(since ?? 0, cursor)
        // A full pull lists every live note, so what failed in it is all
        // that is owed.
        store.refetchPaths = fullPull ? failed : failed.union(owed.subtracting(seen))
        store.refetchSince = retryFrom
    }

    /// What became of one listed snapshot.
    private enum Taken {
        case stored
        /// It did not arrive whole; asking again can help.
        case retry(String)
        /// It arrived as the server holds it and still can't be read. Only a
        /// new version from the desk changes that, and a new version is
        /// listed anyway, so it is not asked for again.
        case unreadable(String)
    }

    /// Downloads, checks, opens and stores one live row. A failure is this
    /// note's alone: the rest of the listing still lands.
    private func take(_ row: FileRow, key: Data) async -> Taken {
        // Pictures are bytes the phone does not draw yet (V39 §7.5): listed,
        // so a full pull keeps nothing stale, never fetched.
        if SyncCore.isSyncableImagePath(row.path, imagesDir: store.config.imagesDir) { return .stored }
        guard let hash = row.contentHash, let blobID = row.blobID else { return .stored }
        if store.contentHash(row.path) == hash { return .stored }
        guard let url = row.downloadURL else { return .retry("no download address") }
        let envelope: Data
        do {
            envelope = try await transport.download(url)
        } catch {
            return .retry("download failed, \(error.localizedDescription)")
        }
        guard SyncCore.contentHash(envelope: envelope) == hash else {
            return .retry("\(envelope.count) bytes arrived but hash differs")
        }
        guard let plaintext = try? SyncCore.open(key: key, context: .file(path: row.path, blobID: blobID), envelope: envelope) else {
            return .unreadable("did not open with this phone's key")
        }
        guard let text = String(data: plaintext, encoding: .utf8) else {
            return .unreadable("not text")
        }
        do {
            try store.applySnapshot(path: row.path, version: row.version, content: text, contentHash: hash, blobID: blobID)
        } catch {
            return .retry("could not be stored, \(error.localizedDescription)")
        }
        return .stored
    }
}
