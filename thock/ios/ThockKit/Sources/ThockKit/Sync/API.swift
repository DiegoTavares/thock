import Foundation

/// The wire shapes of V34 API §6. Unknown fields are ignored and absent
/// optional ones take their documented default, as the contract asks.
public struct DeviceInfo: Codable, Equatable, Sendable {
    public var deviceID: String
    public var role: String
    public var name: String?
    public var pairedAt: String?
    public var lastSeenAt: String?

    enum CodingKeys: String, CodingKey {
        case deviceID = "device_id"
        case role
        case name
        case pairedAt = "paired_at"
        case lastSeenAt = "last_seen_at"
    }
}

public struct WriteCounters: Codable, Equatable, Sendable {
    public var pending: Int?
    public var latestSeq: Int?
    public var ackedThroughSeq: Int?
    public var ackedAtVersion: Int?

    enum CodingKeys: String, CodingKey {
        case pending
        case latestSeq = "latest_seq"
        case ackedThroughSeq = "acked_through_seq"
        case ackedAtVersion = "acked_at_version"
    }
}

public struct VaultInfo: Codable, Equatable, Sendable {
    public var vaultID: String
    public var status: String
    public var keyCheck: String
    public var createdAt: String?
    public var lapsedAt: String?
    public var quotaBytes: Int?
    public var usedBytes: Int?
    public var fileCount: Int?
    public var latestVersion: Int?
    public var writes: WriteCounters?
    public var devices: [DeviceInfo]?

    enum CodingKeys: String, CodingKey {
        case vaultID = "vault_id"
        case status
        case keyCheck = "key_check"
        case createdAt = "created_at"
        case lapsedAt = "lapsed_at"
        case quotaBytes = "quota_bytes"
        case usedBytes = "used_bytes"
        case fileCount = "file_count"
        case latestVersion = "latest_version"
        case writes
        case devices
    }
}

public struct FileRow: Codable, Equatable, Sendable {
    public var path: String
    public var version: Int
    public var deleted: Bool?
    public var blobID: String?
    public var sizeBytes: Int?
    public var contentHash: String?
    public var updatedBy: String?
    public var updatedAt: String?
    public var downloadURL: String?
    public var downloadExpiresAt: String?

    enum CodingKeys: String, CodingKey {
        case path
        case version
        case deleted
        case blobID = "blob_id"
        case sizeBytes = "size_bytes"
        case contentHash = "content_hash"
        case updatedBy = "updated_by"
        case updatedAt = "updated_at"
        case downloadURL = "download_url"
        case downloadExpiresAt = "download_expires_at"
    }
}

public struct FilesPage: Codable, Equatable, Sendable {
    public var files: [FileRow]
    public var nextSince: Int?
    public var hasMore: Bool?

    enum CodingKeys: String, CodingKey {
        case files
        case nextSince = "next_since"
        case hasMore = "has_more"
    }
}

public struct PairResponse: Codable, Equatable, Sendable {
    public var credential: String
    public var device: DeviceInfo
    public var vault: VaultInfo
}

public struct WriteAccepted: Codable, Equatable, Sendable {
    public var seq: Int
    public var createdAt: String?

    enum CodingKeys: String, CodingKey {
        case seq
        case createdAt = "created_at"
    }
}

public struct QueuedWrite: Codable, Equatable, Sendable {
    public var seq: Int
    public var clientID: String
    public var path: String
    public var baseVersion: Int
    public var payload: String
    public var createdAt: String?

    enum CodingKeys: String, CodingKey {
        case seq
        case clientID = "client_id"
        case path
        case baseVersion = "base_version"
        case payload
        case createdAt = "created_at"
    }
}

/// Every non-2xx response: a sentence for the person and a code to branch on.
public struct APIError: Error, Codable, Equatable, Sendable {
    public struct Current: Codable, Equatable, Sendable {
        public var version: Int
    }

    public var status: Int = 0
    public var error: String
    public var code: String
    /// On `409 stale_version`: what the server holds for that path.
    public var current: Current?

    enum CodingKeys: String, CodingKey {
        case error
        case code
        case current
    }

    public init(status: Int, code: String, error: String) {
        self.status = status
        self.code = code
        self.error = error
    }
}

public enum FeedEvent: Equatable, Sendable {
    case file(path: String, version: Int, deleted: Bool)
    case write(seq: Int, path: String)
    case ack(throughSeq: Int, atVersion: Int)
    case vault(status: String)
    case device(deviceID: String, revoked: Bool)

    /// One server-sent event; kinds this build does not know are dropped.
    public init?(event: String, data: String) {
        guard let object = try? JSONSerialization.jsonObject(with: Data(data.utf8)) as? [String: Any] else { return nil }
        switch event {
        case "file":
            guard let path = object["path"] as? String, let version = object["version"] as? Int else { return nil }
            self = .file(path: path, version: version, deleted: object["deleted"] as? Bool ?? false)
        case "write":
            guard let seq = object["seq"] as? Int else { return nil }
            self = .write(seq: seq, path: object["path"] as? String ?? "")
        case "ack":
            guard let through = object["through_seq"] as? Int else { return nil }
            self = .ack(throughSeq: through, atVersion: object["at_version"] as? Int ?? 0)
        case "vault":
            guard let status = object["status"] as? String else { return nil }
            self = .vault(status: status)
        case "device":
            guard let device = object["device_id"] as? String else { return nil }
            self = .device(deviceID: device, revoked: object["revoked"] as? Bool ?? false)
        default:
            return nil
        }
    }
}

public struct HTTPResult: Sendable {
    public var status: Int
    public var body: Data

    public init(status: Int, body: Data) {
        self.status = status
        self.body = body
    }
}

/// What the sync client needs from the network. `HTTPTransport` talks to the
/// Plus backend; `LocalBackend` answers the same requests in-process, so the
/// app runs end to end with no server.
public protocol SyncTransport: Sendable {
    func send(method: String, path: String, query: [String: String], body: Data?, credential: String?) async throws -> HTTPResult
    func download(_ url: String) async throws -> Data
    func upload(_ url: String, body: Data, headers: [String: String]) async throws
    func feed(credential: String) -> AsyncThrowingStream<FeedEvent, Error>
}

/// The pairing QR's one URL (V34 API §5.3):
/// `thock://pair?v=1&code=XXXX-XXXX&key=<base64url>&backend=<base URL>`.
public struct PairingLink: Equatable, Sendable {
    public var code: String
    public var key: Data
    public var backend: String

    public init(code: String, key: Data, backend: String) {
        self.code = code
        self.key = key
        self.backend = backend
    }

    public init?(_ text: String) {
        guard let components = URLComponents(string: text.trimmingCharacters(in: .whitespacesAndNewlines)),
              components.scheme == "thock", components.host == "pair"
        else { return nil }
        var values: [String: String] = [:]
        for item in components.queryItems ?? [] {
            values[item.name] = item.value
        }
        guard (values["v"] ?? "1") == "1",
              let code = values["code"], !code.isEmpty,
              let key = values["key"].flatMap({ Data(base64URL: $0) }), key.count == 32,
              let backend = values["backend"], !backend.isEmpty
        else { return nil }
        self.code = code
        self.key = key
        self.backend = backend
    }

    public var url: String {
        var components = URLComponents()
        components.scheme = "thock"
        components.host = "pair"
        components.queryItems = [
            URLQueryItem(name: "v", value: "1"),
            URLQueryItem(name: "code", value: code),
            URLQueryItem(name: "key", value: key.base64URL),
            URLQueryItem(name: "backend", value: backend),
        ]
        return components.string ?? ""
    }
}

/// Where the vault key and the device credential live. The Keychain on a
/// phone; memory in tests.
public protocol SecretStore: Sendable {
    func secret(_ name: String) -> Data?
    func setSecret(_ name: String, _ value: Data?)
}

public final class MemorySecretStore: SecretStore, @unchecked Sendable {
    private var values: [String: Data] = [:]
    private let lock = NSLock()

    public init() {}

    public func secret(_ name: String) -> Data? {
        lock.lock()
        defer { lock.unlock() }
        return values[name]
    }

    public func setSecret(_ name: String, _ value: Data?) {
        lock.lock()
        defer { lock.unlock() }
        values[name] = value
    }
}

public final class KeychainSecretStore: SecretStore, @unchecked Sendable {
    private let service: String

    public init(service: String = "app.thock.ios.vault") {
        self.service = service
    }

    private func query(_ name: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: name]
    }

    public func secret(_ name: String) -> Data? {
        var query = query(name)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess else { return nil }
        return result as? Data
    }

    public func setSecret(_ name: String, _ value: Data?) {
        SecItemDelete(query(name) as CFDictionary)
        guard let value else { return }
        var attributes = query(name)
        attributes[kSecValueData as String] = value
        // Background refresh needs the key while the phone is locked, and it
        // must never leave this device (V34 §9).
        attributes[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemAdd(attributes as CFDictionary, nil)
    }
}
