import Foundation

/// The sync server of V34 API §6, in memory and in-process. It answers the
/// same requests with the same bodies and status codes as the Plus backend,
/// so the phone's sync runs end to end with no network: in the practice
/// notebook, in tests, and until the real service is up.
public actor LocalBackend: SyncTransport {
    public struct StoredFile: Codable, Equatable, Sendable {
        var version: Int
        var deleted: Bool
        var blobID: String?
        var sizeBytes: Int?
        var contentHash: String?
        var updatedBy: String
        var updatedAt: String
    }

    struct StoredWrite: Codable, Equatable, Sendable {
        var seq: Int
        var clientID: String
        var path: String
        var baseVersion: Int
        var payload: String
        var createdAt: String
    }

    struct Device: Codable, Equatable, Sendable {
        var deviceID: String
        var role: String
        var name: String
        var credential: String
        var pairedAt: String
        var apnsToken: String?
    }

    struct Upload: Codable, Equatable, Sendable {
        var path: String
        var sizeBytes: Int
        var contentHash: String
    }

    public struct State: Codable, Equatable, Sendable {
        var vaultID: String?
        var keyCheck = ""
        var createdAt = ""
        var lapsedAt: String?
        var quotaBytes = 209_715_200
        var latestVersion = 0
        var latestSeq = 0
        var ackedThroughSeq = 0
        var ackedAtVersion = 0
        var devices: [Device] = []
        var files: [String: StoredFile] = [:]
        var blobs: [String: Data] = [:]
        var uploads: [String: Upload] = [:]
        var writes: [StoredWrite] = []
        var everAccepted: [String: Int] = [:]
        var pairingCode: String?
        var pairingExpires: Date?
        var oldestTombstoneCursor = 0

        public init() {}
    }

    public private(set) var state: State
    private var listeners: [String: [UUID: AsyncThrowingStream<FeedEvent, Error>.Continuation]] = [:]
    private var onChange: (@Sendable (State) -> Void)?
    /// Set to make every call fail the way a phone with no signal would.
    public var isOffline = false
    private let now: @Sendable () -> Date

    public init(state: State = State(), now: @escaping @Sendable () -> Date = { Date() }) {
        self.state = state
        self.now = now
    }

    public func setOffline(_ offline: Bool) {
        isOffline = offline
        if offline {
            for (_, group) in listeners {
                for (_, continuation) in group {
                    continuation.finish(throwing: URLError(.notConnectedToInternet))
                }
            }
            listeners = [:]
        }
    }

    public func observe(_ onChange: @escaping @Sendable (State) -> Void) {
        self.onChange = onChange
    }

    /// Ends or restores the Plus entitlement, to exercise the read-only phone.
    public func setLapsed(_ lapsed: Bool) {
        state.lapsedAt = lapsed ? timestamp() : nil
        emit(.vault(status: lapsed ? "lapsed" : "active"), from: "server")
        saved()
    }

    private func saved() {
        onChange?(state)
    }

    private func timestamp() -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.string(from: now())
    }

    private static func randomHex(_ bytes: Int) -> String {
        (0..<bytes).map { _ in String(format: "%02x", UInt8.random(in: 0...255)) }.joined()
    }

    // MARK: Feed

    public nonisolated func feed(credential: String) -> AsyncThrowingStream<FeedEvent, Error> {
        AsyncThrowingStream { continuation in
            let id = UUID()
            Task {
                await self.listen(credential: credential, id: id, continuation: continuation)
            }
            continuation.onTermination = { _ in
                Task { await self.forget(id) }
            }
        }
    }

    private func listen(credential: String, id: UUID, continuation: AsyncThrowingStream<FeedEvent, Error>.Continuation) {
        guard !isOffline, let device = state.devices.first(where: { $0.credential == credential }) else {
            continuation.finish(throwing: URLError(.notConnectedToInternet))
            return
        }
        listeners[device.role, default: [:]][id] = continuation
    }

    private func forget(_ id: UUID) {
        for role in listeners.keys {
            listeners[role]?[id] = nil
        }
    }

    /// Events go to the other device only.
    private func emit(_ event: FeedEvent, from role: String) {
        for (listenerRole, group) in listeners where listenerRole != role {
            for (_, continuation) in group {
                continuation.yield(event)
            }
        }
    }

    // MARK: Blobs

    public func download(_ url: String) async throws -> Data {
        guard !isOffline else { throw URLError(.notConnectedToInternet) }
        guard url.hasPrefix("thock-local://blob/"), let blob = state.blobs[String(url.dropFirst("thock-local://blob/".count))] else {
            throw URLError(.fileDoesNotExist)
        }
        return blob
    }

    public func upload(_ url: String, body: Data, headers: [String: String]) async throws {
        guard !isOffline else { throw URLError(.notConnectedToInternet) }
        guard url.hasPrefix("thock-local://upload/") else { throw URLError(.badURL) }
        state.blobs[String(url.dropFirst("thock-local://upload/".count))] = body
    }

    // MARK: Routes

    struct Reply: Error {
        var status: Int
        var json: [String: Any]

        static func error(_ status: Int, _ code: String, _ sentence: String, extra: [String: Any] = [:]) -> Reply {
            var json: [String: Any] = ["error": sentence, "code": code]
            for (key, value) in extra {
                json[key] = value
            }
            return Reply(status: status, json: json)
        }
    }

    public func send(method: String, path: String, query: [String: String], body: Data?, credential: String?) async throws -> HTTPResult {
        guard !isOffline else { throw URLError(.notConnectedToInternet) }
        let reply: Reply
        do {
            let object = try body.map { try JSONSerialization.jsonObject(with: $0) as? [String: Any] ?? [:] } ?? [:]
            reply = try route(method: method, path: path, query: query, body: object, credential: credential)
        } catch let refusal as Reply {
            reply = refusal
        } catch {
            reply = .error(400, "bad_request", "That request could not be read.")
        }
        saved()
        if reply.status == 204 {
            return HTTPResult(status: 204, body: Data())
        }
        return HTTPResult(status: reply.status, body: try JSONSerialization.data(withJSONObject: reply.json))
    }

    private func route(method: String, path: String, query: [String: String], body: [String: Any], credential: String?) throws -> Reply {
        if method == "POST", path == "/v1/vault/pair" {
            return try pair(body)
        }
        guard let credential else {
            throw Reply.error(401, "unauthorized", "Connect this device first.")
        }
        if method == "POST", path == "/v1/vault", credential.hasPrefix("tpk_") {
            return try createVault(body, credential: credential)
        }
        guard state.vaultID != nil else {
            if credential.hasPrefix("tpk_") {
                throw Reply.error(404, "vault_missing", "There is no vault to sync yet.")
            }
            throw Reply.error(401, "unauthorized", "Connect again at the desk.")
        }
        guard let device = state.devices.first(where: { $0.credential == credential }) else {
            throw Reply.error(401, "unauthorized", "Connect again at the desk.")
        }
        let role = device.role
        let lapsed = state.lapsedAt != nil
        func only(_ wanted: String) throws {
            guard role == wanted else {
                throw Reply.error(403, "role_forbidden", "That is for the other device.")
            }
        }
        func refuseWhenLapsed() throws {
            if lapsed {
                throw Reply.error(403, "plus_lapsed", "Thock Plus has ended. Renew it to keep writing from your phone.")
            }
        }

        switch (method, path) {
        case ("GET", "/v1/vault"):
            return Reply(status: 200, json: vaultObject())
        case ("DELETE", "/v1/vault"):
            try only("desk")
            emit(.vault(status: "deleted"), from: role)
            state = State()
            return Reply(status: 204, json: [:])
        case ("POST", "/v1/vault/reset"):
            try only("desk")
            try refuseWhenLapsed()
            guard let check = body["key_check"] as? String else { throw Reply.error(400, "bad_request", "A key check is required.") }
            state.files = [:]
            state.blobs = [:]
            state.uploads = [:]
            state.writes = []
            state.devices.removeAll { $0.role == "phone" }
            state.keyCheck = check
            return Reply(status: 200, json: vaultObject())
        case ("POST", "/v1/vault/pairings"):
            try only("desk")
            try refuseWhenLapsed()
            let alphabet = Array("ABCDEFGHJKLMNPQRSTUVWXYZ23456789")
            let code = String((0..<8).map { _ in alphabet[Int.random(in: 0..<alphabet.count)] })
            state.pairingCode = code
            state.pairingExpires = now().addingTimeInterval(600)
            let formatter = ISO8601DateFormatter()
            return Reply(status: 201, json: ["code": String(code.prefix(4)) + "-" + String(code.suffix(4)), "expires_at": formatter.string(from: state.pairingExpires ?? now())])
        case ("GET", "/v1/vault/devices"):
            return Reply(status: 200, json: ["devices": state.devices.map(deviceObject)])
        case ("PATCH", "/v1/vault/devices/me"):
            try only("phone")
            try refuseWhenLapsed()
            guard let index = state.devices.firstIndex(where: { $0.credential == credential }) else {
                throw Reply.error(404, "not_found", "That device is gone.")
            }
            if let name = body["device_name"] as? String { state.devices[index].name = name }
            if body.keys.contains("apns_token") { state.devices[index].apnsToken = body["apns_token"] as? String }
            return Reply(status: 200, json: deviceObject(state.devices[index]))
        case ("GET", "/v1/vault/files"):
            return try listFiles(query)
        case ("POST", "/v1/vault/writes"):
            try only("phone")
            try refuseWhenLapsed()
            return try acceptWrite(body)
        case ("GET", "/v1/vault/writes"):
            try only("desk")
            try refuseWhenLapsed()
            let after = Int(query["after"] ?? "") ?? state.ackedThroughSeq
            let limit = min(Int(query["limit"] ?? "") ?? 200, 1000)
            let rows = state.writes.filter { $0.seq > after }.sorted { $0.seq < $1.seq }
            return Reply(status: 200, json: [
                "writes": rows.prefix(limit).map { ["seq": $0.seq, "client_id": $0.clientID, "path": $0.path, "base_version": $0.baseVersion, "payload": $0.payload, "created_at": $0.createdAt] },
                "has_more": rows.count > limit,
            ])
        case ("POST", "/v1/vault/writes/ack"):
            try only("desk")
            try refuseWhenLapsed()
            guard let through = body["through_seq"] as? Int else { throw Reply.error(400, "bad_request", "through_seq is required.") }
            if through > state.ackedThroughSeq {
                state.writes.removeAll { $0.seq <= through }
                state.ackedThroughSeq = through
                state.ackedAtVersion = state.latestVersion
                emit(.ack(throughSeq: through, atVersion: state.ackedAtVersion), from: role)
            }
            return Reply(status: 200, json: ["through_seq": state.ackedThroughSeq, "at_version": state.ackedAtVersion])
        default:
            break
        }

        if path.hasPrefix("/v1/vault/devices/"), path.hasSuffix("/revoke"), method == "POST" {
            try only("desk")
            let deviceID = String(path.dropFirst("/v1/vault/devices/".count).dropLast("/revoke".count))
            guard let target = state.devices.first(where: { $0.deviceID == deviceID }) else {
                throw Reply.error(404, "not_found", "That device is gone.")
            }
            guard target.role == "phone" else { throw Reply.error(400, "bad_request", "Turn sync off to remove the desk.") }
            state.devices.removeAll { $0.deviceID == deviceID }
            emit(.device(deviceID: deviceID, revoked: true), from: role)
            return Reply(status: 204, json: [:])
        }

        if path.hasPrefix("/v1/vault/files/") {
            try only("desk")
            try refuseWhenLapsed()
            var tail = String(path.dropFirst("/v1/vault/files/".count))
            let committing = method == "POST" && tail.hasSuffix("/commit")
            if committing {
                tail = String(tail.dropLast("/commit".count))
            }
            let filePath = tail.split(separator: "/", omittingEmptySubsequences: false).map { $0.removingPercentEncoding ?? String($0) }.joined(separator: "/")
            guard SyncCore.isSyncablePath(filePath) else {
                throw Reply.error(422, "path_not_allowed", "That kind of file is not kept for your phone.")
            }
            switch (method, committing) {
            case ("POST", false): return try beginUpload(filePath, body)
            case ("POST", true): return try commit(filePath, body, role: role)
            case ("DELETE", _): return try tombstone(filePath, body, role: role)
            default: break
            }
        }
        throw Reply.error(404, "not_found", "There is nothing at that address.")
    }

    private func deviceObject(_ device: Device) -> [String: Any] {
        ["device_id": device.deviceID, "role": device.role, "name": device.name, "paired_at": device.pairedAt, "last_seen_at": timestamp()]
    }

    private func vaultObject() -> [String: Any] {
        let live = state.files.values.filter { !$0.deleted }
        return [
            "vault_id": state.vaultID ?? "",
            "status": state.lapsedAt == nil ? "active" : "lapsed",
            "key_check": state.keyCheck,
            "created_at": state.createdAt,
            "lapsed_at": state.lapsedAt as Any? ?? NSNull(),
            "quota_bytes": state.quotaBytes,
            "used_bytes": live.reduce(0) { $0 + ($1.sizeBytes ?? 0) },
            "file_count": live.count,
            "latest_version": state.latestVersion,
            "writes": [
                "pending": state.writes.count,
                "latest_seq": state.latestSeq,
                "acked_through_seq": state.ackedThroughSeq,
                "acked_at_version": state.ackedAtVersion,
            ],
            "devices": state.devices.map(deviceObject),
        ]
    }

    private func createVault(_ body: [String: Any], credential: String) throws -> Reply {
        guard let check = body["key_check"] as? String, check.count == 32 else {
            throw Reply.error(400, "bad_request", "A key check is required.")
        }
        if state.vaultID == nil {
            state.vaultID = Self.randomHex(8)
            state.createdAt = timestamp()
            state.keyCheck = check
            state.devices.append(Device(deviceID: Self.randomHex(8), role: "desk", name: body["device_name"] as? String ?? "Desk", credential: credential, pairedAt: timestamp()))
            return Reply(status: 200, json: vaultObject())
        }
        guard state.devices.contains(where: { $0.role == "desk" && $0.credential == credential }) else {
            throw Reply.error(409, "device_exists", "Another computer already syncs this vault.")
        }
        if state.keyCheck != check {
            guard state.files.values.allSatisfy(\.deleted) else {
                throw Reply.error(409, "key_mismatch", "This vault was set up with a different key.")
            }
            state.keyCheck = check
        }
        return Reply(status: 200, json: vaultObject())
    }

    private func pair(_ body: [String: Any]) throws -> Reply {
        let offered = (body["code"] as? String ?? "").replacingOccurrences(of: "-", with: "").uppercased()
        guard let code = state.pairingCode, !offered.isEmpty, offered == code else {
            throw Reply.error(404, "pairing_invalid", "That code didn't work. Try again.")
        }
        guard let expires = state.pairingExpires, expires > now() else {
            throw Reply.error(410, "pairing_expired", "That code has expired. Ask the desk for a new one.")
        }
        state.pairingCode = nil
        // A second phone replaces the first.
        for old in state.devices where old.role == "phone" {
            emit(.device(deviceID: old.deviceID, revoked: true), from: "server")
        }
        state.devices.removeAll { $0.role == "phone" }
        let device = Device(deviceID: Self.randomHex(8), role: "phone", name: body["device_name"] as? String ?? "iPhone",
                            credential: "tpp_" + Self.randomHex(24), pairedAt: timestamp(), apnsToken: body["apns_token"] as? String)
        state.devices.append(device)
        return Reply(status: 201, json: [
            "credential": device.credential,
            "device": ["device_id": device.deviceID, "role": "phone", "name": device.name],
            "vault": vaultObject(),
        ])
    }

    private func listFiles(_ query: [String: String]) throws -> Reply {
        let limit = min(max(Int(query["limit"] ?? "") ?? 500, 1), 2000)
        var rows = state.files.map { (path: $0.key, file: $0.value) }
        if let since = query["since"].flatMap(Int.init) {
            guard since >= state.oldestTombstoneCursor else {
                throw Reply.error(410, "cursor_expired", "Too much has changed. Starting over.")
            }
            rows = rows.filter { $0.file.version > since }
        } else {
            rows = rows.filter { !$0.file.deleted }
        }
        rows.sort { $0.file.version < $1.file.version }
        let page = rows.prefix(limit)
        let expires = ISO8601DateFormatter().string(from: now().addingTimeInterval(900))
        let files: [[String: Any]] = page.map { row in
            var object: [String: Any] = ["path": row.path, "version": row.file.version, "deleted": row.file.deleted, "updated_by": row.file.updatedBy, "updated_at": row.file.updatedAt]
            if !row.file.deleted, let blobID = row.file.blobID {
                object["blob_id"] = blobID
                object["size_bytes"] = row.file.sizeBytes ?? 0
                object["content_hash"] = row.file.contentHash ?? ""
                object["download_url"] = "thock-local://blob/" + blobID
                object["download_expires_at"] = expires
            }
            return object
        }
        let hasMore = rows.count > limit
        let next = hasMore ? page.last?.file.version ?? state.latestVersion : state.latestVersion
        return Reply(status: 200, json: ["files": files, "next_since": next, "has_more": hasMore])
    }

    private func current(_ path: String) -> [String: Any] {
        guard let file = state.files[path] else { return ["version": 0, "deleted": false] }
        return ["version": file.version, "blob_id": file.blobID ?? "", "content_hash": file.contentHash ?? "", "deleted": file.deleted]
    }

    private func checkExpected(_ path: String, _ body: [String: Any]) throws {
        let expected = body["expected_version"] as? Int ?? 0
        guard expected == state.files[path]?.version ?? 0 else {
            throw Reply.error(409, "stale_version", "That note changed since it was last read.", extra: ["current": current(path)])
        }
    }

    private func beginUpload(_ path: String, _ body: [String: Any]) throws -> Reply {
        guard let blobID = body["blob_id"] as? String, blobID.count == 32,
              let size = body["size_bytes"] as? Int, let hash = body["content_hash"] as? String, hash.count == 64
        else { throw Reply.error(400, "bad_request", "The upload is missing its blob id, size or hash.") }
        guard size <= 2_097_152 + 32 else { throw Reply.error(413, "too_large", "That note is too large to keep for your phone.") }
        try checkExpected(path, body)
        let used = state.files.values.filter { !$0.deleted }.reduce(0) { $0 + ($1.sizeBytes ?? 0) }
        guard used + size <= state.quotaBytes + state.quotaBytes / 10 else {
            throw Reply.error(507, "quota_exceeded", "The vault is over its space for your phone.")
        }
        state.uploads[blobID] = Upload(path: path, sizeBytes: size, contentHash: hash)
        let expires = ISO8601DateFormatter().string(from: now().addingTimeInterval(900))
        return Reply(status: 200, json: ["upload": ["url": "thock-local://upload/" + blobID, "method": "PUT", "headers": ["Content-Type": "application/octet-stream"], "expires_at": expires]])
    }

    private func commit(_ path: String, _ body: [String: Any], role: String) throws -> Reply {
        guard let blobID = body["blob_id"] as? String else { throw Reply.error(400, "bad_request", "A blob id is required.") }
        if let file = state.files[path], file.blobID == blobID, !file.deleted {
            return Reply(status: 200, json: ["version": file.version, "updated_at": file.updatedAt])
        }
        guard let upload = state.uploads[blobID], upload.path == path, let blob = state.blobs[blobID], blob.count == upload.sizeBytes else {
            throw Reply.error(422, "blob_missing", "That upload never arrived.")
        }
        try checkExpected(path, body)
        if let previous = state.files[path]?.blobID {
            state.blobs[previous] = nil
        }
        state.latestVersion += 1
        let file = StoredFile(version: state.latestVersion, deleted: false, blobID: blobID, sizeBytes: upload.sizeBytes, contentHash: upload.contentHash, updatedBy: role, updatedAt: timestamp())
        state.files[path] = file
        state.uploads[blobID] = nil
        emit(.file(path: path, version: file.version, deleted: false), from: role)
        return Reply(status: 200, json: ["version": file.version, "updated_at": file.updatedAt])
    }

    private func tombstone(_ path: String, _ body: [String: Any], role: String) throws -> Reply {
        guard let file = state.files[path] else { throw Reply.error(404, "not_found", "There is no such note.") }
        if file.deleted {
            return Reply(status: 200, json: ["version": file.version])
        }
        try checkExpected(path, body)
        if let blobID = file.blobID {
            state.blobs[blobID] = nil
        }
        state.latestVersion += 1
        state.files[path] = StoredFile(version: state.latestVersion, deleted: true, blobID: nil, sizeBytes: nil, contentHash: nil, updatedBy: role, updatedAt: timestamp())
        emit(.file(path: path, version: state.latestVersion, deleted: true), from: role)
        return Reply(status: 200, json: ["version": state.latestVersion])
    }

    private func acceptWrite(_ body: [String: Any]) throws -> Reply {
        guard let clientID = body["client_id"] as? String, let path = body["path"] as? String, let payload = body["payload"] as? String else {
            throw Reply.error(400, "bad_request", "The write is missing its id, path or payload.")
        }
        // The same client id again is the same write: retries are free.
        if let seq = state.everAccepted[clientID] {
            let created = state.writes.first { $0.seq == seq }?.createdAt ?? timestamp()
            return Reply(status: 200, json: ["seq": seq, "created_at": created])
        }
        guard SyncCore.isSyncablePath(path) else {
            throw Reply.error(422, "path_not_allowed", "That kind of file is not kept for your phone.")
        }
        guard (Data(base64Encoded: payload)?.count ?? .max) <= 2_097_152 else {
            throw Reply.error(413, "too_large", "That is too much to send at once.")
        }
        state.latestSeq += 1
        let write = StoredWrite(seq: state.latestSeq, clientID: clientID, path: path, baseVersion: body["base_version"] as? Int ?? 0, payload: payload, createdAt: timestamp())
        state.writes.append(write)
        state.everAccepted[clientID] = write.seq
        emit(.write(seq: write.seq, path: path), from: "phone")
        return Reply(status: 201, json: ["seq": write.seq, "created_at": write.createdAt])
    }
}
