import CryptoKit
import Foundation

public enum SealContext: Equatable, Sendable {
    case file(path: String, blobID: String)
    case write(clientID: String)

    var associatedData: Data {
        var data = Data()
        switch self {
        case .file(let path, let blobID):
            data.append(Data("thock-file/1".utf8))
            data.append(0)
            data.append(Data(path.utf8))
            data.append(0)
            data.append(Data(blobID.utf8))
        case .write(let clientID):
            data.append(Data("thock-write/1".utf8))
            data.append(0)
            data.append(Data(clientID.utf8))
        }
        return data
    }
}

public enum SealError: Error, Equatable {
    case badKey
    case badEnvelope
    case authentication
}

extension SyncCore {
    static let envelopeMagic = Data("TVS1".utf8)

    /// `"TVS1" ‖ nonce ‖ ciphertext ‖ tag`, ChaCha20-Poly1305 with the vault
    /// key (V34 API §5).
    public static func seal(key: Data, context: SealContext, plaintext: Data, nonce: Data? = nil) throws -> Data {
        guard key.count == 32 else { throw SealError.badKey }
        let chosen = try nonce.map { try ChaChaPoly.Nonce(data: $0) } ?? ChaChaPoly.Nonce()
        let box = try ChaChaPoly.seal(plaintext, using: SymmetricKey(data: key), nonce: chosen, authenticating: context.associatedData)
        return envelopeMagic + box.combined
    }

    public static func open(key: Data, context: SealContext, envelope: Data) throws -> Data {
        guard key.count == 32 else { throw SealError.badKey }
        guard envelope.count >= 4 + 12 + 16, envelope.prefix(4) == envelopeMagic else { throw SealError.badEnvelope }
        do {
            let box = try ChaChaPoly.SealedBox(combined: envelope.dropFirst(4))
            return try ChaChaPoly.open(box, using: SymmetricKey(data: key), authenticating: context.associatedData)
        } catch {
            throw SealError.authentication
        }
    }

    public static func contentHash(envelope: Data) -> String {
        sha256Hex(envelope)
    }

    /// The only key-derived value the server ever receives.
    public static func keyCheck(key: Data) -> String {
        String(sha256Hex(Data("thock-vault-key-check/1".utf8) + key).prefix(32))
    }

    static let syncableExtensions: Set<String> = ["md", "txt", "toml", "json", "csv"]
    static let excludedPrefixes = [".thock/history/", ".thock/cache/", ".thock/sync/", ".git/"]

    /// The path rules of V34 API §4.1.
    public static func isSyncablePath(_ path: String) -> Bool {
        guard !path.isEmpty, path.utf8.count <= 1024 else { return false }
        guard path == path.precomposedStringWithCanonicalMapping else { return false }
        guard !path.contains("\\"), !path.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }) else { return false }
        let segments = path.split(separator: "/", omittingEmptySubsequences: false)
        guard !segments.contains(where: { $0.isEmpty || $0 == "." || $0 == ".." }) else { return false }
        guard let last = segments.last, let dot = last.lastIndex(of: "."), dot != last.startIndex else { return false }
        let fileExtension = last[last.index(after: dot)...].lowercased()
        guard syncableExtensions.contains(fileExtension) else { return false }
        return !excludedPrefixes.contains { path.hasPrefix($0) }
    }
}

extension Data {
    public init?(hex: String) {
        guard hex.count % 2 == 0 else { return nil }
        var data = Data(capacity: hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let byte = UInt8(hex[index..<next], radix: 16) else { return nil }
            data.append(byte)
            index = next
        }
        self = data
    }

    public var hex: String {
        map { String(format: "%02x", $0) }.joined()
    }

    public init?(base64URL: String) {
        var text = base64URL.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while text.count % 4 != 0 {
            text.append("=")
        }
        self.init(base64Encoded: text)
    }

    public var base64URL: String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
