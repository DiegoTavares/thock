import Foundation

public struct HeadingRef: Codable, Equatable, Sendable {
    public var text: String
    public var level: Int
    public var ordinal: Int

    public init(text: String, level: Int = 2, ordinal: Int = 0) {
        self.text = text
        self.level = level
        self.ordinal = ordinal
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        text = try container.decode(String.self, forKey: .text)
        level = try container.decodeIfPresent(Int.self, forKey: .level) ?? 2
        ordinal = try container.decodeIfPresent(Int.self, forKey: .ordinal) ?? 0
    }
}

public enum WriteKind: String, Codable, Sendable {
    case create
    case append
    case replaceLine = "replace_line"
    case removeLine = "remove_line"
    case replaceSection = "replace_section"
}

public enum Placement: String, Codable, Sendable {
    case end
    case beforeChildren = "before_children"
}

public enum WriteError: Error, Equatable {
    case malformed(String)
}

/// The plaintext inside a write envelope (V34 API §7): one operation from the
/// phone's write contract, as data.
public struct WriteDocument: Equatable, Sendable {
    public var version: Int = 1
    public var clientID: String
    public var kind: WriteKind
    public var path: String
    public var madeAt: String
    public var deviceID: String

    public var content: String?
    public var heading: HeadingRef?
    public var lines: [String] = []
    public var placement: Placement = .end
    public var blankLineBefore = false
    public var createFromTemplate = false
    public var lineHash: String?
    public var ordinal = 0
    public var newLine: String?
    public var baseHash: String?

    public init(clientID: String, kind: WriteKind, path: String, madeAt: String, deviceID: String) {
        self.clientID = clientID
        self.kind = kind
        self.path = path
        self.madeAt = madeAt
        self.deviceID = deviceID
    }

    public static func parse(_ json: String) throws -> WriteDocument {
        guard let data = json.data(using: .utf8) else { throw WriteError.malformed("not UTF-8") }
        do {
            let document = try JSONDecoder().decode(WriteDocument.self, from: data)
            try document.validate()
            return document
        } catch let error as WriteError {
            throw error
        } catch {
            throw WriteError.malformed(String(describing: error))
        }
    }

    public func json() -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(self), let text = String(data: data, encoding: .utf8) else {
            return "{}"
        }
        return text
    }

    func validate() throws {
        guard version == 1 else { throw WriteError.malformed("unknown version \(version)") }
        let single = [newLine].compactMap { $0 } + lines
        if single.contains(where: { $0.contains("\n") || $0.contains("\r") }) {
            throw WriteError.malformed("line with a terminator")
        }
        switch kind {
        case .create:
            guard content != nil else { throw WriteError.malformed("create without content") }
        case .append:
            break
        case .replaceLine:
            // A null heading is the whole file, as for an append.
            guard lineHash != nil, newLine != nil else { throw WriteError.malformed("replace_line incomplete") }
        case .removeLine:
            guard lineHash != nil else { throw WriteError.malformed("remove_line incomplete") }
        case .replaceSection:
            guard heading != nil, baseHash != nil else { throw WriteError.malformed("replace_section incomplete") }
        }
    }
}

extension WriteDocument: Codable {
    enum CodingKeys: String, CodingKey {
        case version = "v"
        case clientID = "client_id"
        case kind
        case path
        case madeAt = "made_at"
        case deviceID = "device_id"
        case content
        case heading
        case lines
        case placement
        case blankLineBefore = "blank_line_before"
        case createFromTemplate = "create_from_template"
        case lineHash = "line_hash"
        case ordinal
        case newLine = "new_line"
        case baseHash = "base_hash"
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        version = try container.decodeIfPresent(Int.self, forKey: .version) ?? 1
        clientID = try container.decode(String.self, forKey: .clientID)
        kind = try container.decode(WriteKind.self, forKey: .kind)
        path = try container.decode(String.self, forKey: .path)
        madeAt = try container.decodeIfPresent(String.self, forKey: .madeAt) ?? ""
        deviceID = try container.decodeIfPresent(String.self, forKey: .deviceID) ?? ""
        content = try container.decodeIfPresent(String.self, forKey: .content)
        heading = try container.decodeIfPresent(HeadingRef.self, forKey: .heading)
        lines = try container.decodeIfPresent([String].self, forKey: .lines) ?? []
        placement = try container.decodeIfPresent(Placement.self, forKey: .placement) ?? .end
        blankLineBefore = try container.decodeIfPresent(Bool.self, forKey: .blankLineBefore) ?? false
        createFromTemplate = try container.decodeIfPresent(Bool.self, forKey: .createFromTemplate) ?? false
        lineHash = try container.decodeIfPresent(String.self, forKey: .lineHash)
        ordinal = try container.decodeIfPresent(Int.self, forKey: .ordinal) ?? 0
        newLine = try container.decodeIfPresent(String.self, forKey: .newLine)
        baseHash = try container.decodeIfPresent(String.self, forKey: .baseHash)
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(version, forKey: .version)
        try container.encode(clientID, forKey: .clientID)
        try container.encode(kind, forKey: .kind)
        try container.encode(path, forKey: .path)
        try container.encode(madeAt, forKey: .madeAt)
        try container.encode(deviceID, forKey: .deviceID)
        switch kind {
        case .create:
            try container.encode(content ?? "", forKey: .content)
        case .append:
            // An explicit null is the contract's "end of the file".
            try container.encode(heading, forKey: .heading)
            try container.encode(lines, forKey: .lines)
            try container.encode(placement, forKey: .placement)
            try container.encode(blankLineBefore, forKey: .blankLineBefore)
            try container.encode(createFromTemplate, forKey: .createFromTemplate)
        case .replaceLine:
            try container.encode(heading, forKey: .heading)
            try container.encode(lineHash, forKey: .lineHash)
            try container.encode(ordinal, forKey: .ordinal)
            try container.encode(newLine, forKey: .newLine)
        case .removeLine:
            try container.encode(heading, forKey: .heading)
            try container.encode(lineHash, forKey: .lineHash)
            try container.encode(ordinal, forKey: .ordinal)
        case .replaceSection:
            try container.encode(heading, forKey: .heading)
            try container.encode(baseHash, forKey: .baseHash)
            try container.encode(lines, forKey: .lines)
        }
    }
}
