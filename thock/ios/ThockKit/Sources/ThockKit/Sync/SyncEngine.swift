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
    private var running = false
    private var again = false

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
        set(.working)
        await sync()
    }

    /// Sends a problem report to the service, which files it for a person
    /// to read (V39). Returns the report's number.
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
        stop()
        secrets.setSecret(Self.keyName, nil)
        secrets.setSecret(Self.credentialName, nil)
        store.wipe()
        set(.notConnected)
    }

    public func updatePushToken(_ token: String?) async {
        guard let credential else { return }
        _ = try? await call("PATCH", "/v1/vault/devices/me", body: ["apns_token": token], credential: credential, as: DeviceInfo.self)
    }

    // MARK: The loop

    /// Holds the feed while the app is in the foreground. Events are nudges:
    /// every connect and reconnect starts with a full round regardless.
    public func start() {
        guard feedTask == nil, store.isConnected else { return }
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

    public func stop() {
        feedTask?.cancel()
        feedTask = nil
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
    /// has applied. Safe to call from anywhere, any number of times.
    public func sync() async {
        guard store.isConnected else { return }
        if running {
            again = true
            return
        }
        running = true
        defer { running = false }
        repeat {
            again = false
            await round()
        } while again
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
            if !store.isReadOnly {
                try await flush(credential: credential, key: key)
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
        // the desk no longer has is removed here too.
        let retrying = store.refetchPaths
        var since: Int? = store.cursor > 0 && retrying.isEmpty ? store.cursor : nil
        var fullPull = since == nil
        var listed: Set<String> = []
        var failed: Set<String> = []
        while true {
            var query = ["limit": "500"]
            if let since {
                query["since"] = String(since)
            }
            let page: FilesPage
            do {
                page = try await call("GET", "/v1/vault/files", query: query, credential: credential, as: FilesPage.self)
            } catch let error as APIError where error.code == "cursor_expired" {
                since = nil
                fullPull = true
                listed = []
                continue
            }
            for row in page.files {
                if row.deleted == true {
                    try store.applyTombstone(path: row.path, version: row.version)
                    continue
                }
                listed.insert(row.path)
                guard let hash = row.contentHash, let blobID = row.blobID else { continue }
                if store.contentHash(row.path) == hash {
                    continue
                }
                guard let url = row.downloadURL else {
                    diagnostics.failures.append("\(row.path): no download address")
                    failed.insert(row.path)
                    continue
                }
                // A blob that does not arrive, hash or open is a corrupt
                // version, to fetch again on the next round, never a note
                // to show.
                let envelope: Data
                do {
                    envelope = try await transport.download(url)
                } catch {
                    diagnostics.failures.append("\(row.path): download failed, \(error.localizedDescription)")
                    failed.insert(row.path)
                    continue
                }
                guard SyncCore.contentHash(envelope: envelope) == hash else {
                    diagnostics.failures.append("\(row.path): \(envelope.count) bytes arrived but hash differs")
                    failed.insert(row.path)
                    continue
                }
                guard let plaintext = try? SyncCore.open(key: key, context: .file(path: row.path, blobID: blobID), envelope: envelope) else {
                    diagnostics.failures.append("\(row.path): did not open with this phone's key")
                    failed.insert(row.path)
                    continue
                }
                guard let text = String(data: plaintext, encoding: .utf8) else {
                    diagnostics.failures.append("\(row.path): not text")
                    failed.insert(row.path)
                    continue
                }
                try store.applySnapshot(path: row.path, version: row.version, content: text, contentHash: hash, blobID: blobID)
            }
            since = page.nextSince ?? page.files.map(\.version).max() ?? since ?? 0
            if page.hasMore != true { break }
        }
        // A path whose snapshot failed was still listed, so it survives this;
        // desk deletions keep arriving while it is fetched again.
        if fullPull {
            try store.removeFiles(notIn: listed)
        }
        store.cursor = since ?? 0
        store.refetchPaths = failed
    }
}
