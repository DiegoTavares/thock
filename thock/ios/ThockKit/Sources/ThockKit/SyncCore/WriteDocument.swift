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
    case moveBlock = "move_block"
    case removeBlock = "remove_block"
    /// Renames a whole file (V40 §6). The only kind that touches two paths:
    /// `path` is the source and `toPath` the destination.
    case moveFile = "move_file"
    /// Creates a picture in the images folder from its bytes (V39 §6.2).
    /// The phone sends these; it never applies one to its text store.
    case putFile = "put_file"
}

/// Where a `move_block` lands inside its destination group (V38 §7.1).
public enum Place: Equatable, Sendable {
    /// Before the group's first body line.
    case top
    /// After the group's own lines, above its first subsection.
    case end
    /// After the named line and its indented continuation.
    case after(lineHash: String, ordinal: Int)
}

extension Place: Codable {
    enum CodingKeys: String, CodingKey {
        case after
    }

    enum AfterKeys: String, CodingKey {
        case lineHash = "line_hash"
        case ordinal
    }

    public init(from decoder: Decoder) throws {
        if let single = try? decoder.singleValueContainer(), let name = try? single.decode(String.self) {
            switch name {
            case "top": self = .top
            case "end": self = .end
            default: throw WriteError.malformed("unknown place \(name)")
            }
            return
        }
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let after = try container.nestedContainer(keyedBy: AfterKeys.self, forKey: .after)
        self = .after(lineHash: try after.decode(String.self, forKey: .lineHash), ordinal: try after.decodeIfPresent(Int.self, forKey: .ordinal) ?? 0)
    }

    public func encode(to encoder: Encoder) throws {
        switch self {
        case .top:
            var single = encoder.singleValueContainer()
            try single.encode("top")
        case .end:
            var single = encoder.singleValueContainer()
            try single.encode("end")
        case .after(let lineHash, let ordinal):
            var container = encoder.container(keyedBy: CodingKeys.self)
            var after = container.nestedContainer(keyedBy: AfterKeys.self, forKey: .after)
            try after.encode(lineHash, forKey: .lineHash)
            try after.encode(ordinal, forKey: .ordinal)
        }
    }
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
    /// `move_block`: the destination group, where in it, and the section a
    /// missing destination is created at the end of.
    public var to: HeadingRef?
    public var place: Place = .end
    public var createUnder: HeadingRef?
    /// `move_file`: where the file goes. Shares the `to` key with
    /// `move_block`, which reads it as a heading.
    public var toPath: String?
    /// `put_file`: the picture's bytes and the SHA-256 of them, hex.
    public var contentBase64: String?
    public var contentHash: String?

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
        guard !clientID.trimmingCharacters(in: .whitespaces).isEmpty else { throw WriteError.malformed("no client id") }
        guard !path.trimmingCharacters(in: .whitespaces).isEmpty else { throw WriteError.malformed("no path") }
        // The desk reads these as unsigned numbers and refuses the write
        // otherwise, so the phone must not apply it either.
        guard ordinal >= 0 else { throw WriteError.malformed("negative ordinal") }
        for heading in [heading, to, createUnder].compactMap({ $0 }) {
            guard !heading.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { throw WriteError.malformed("empty heading") }
            guard (0...255).contains(heading.level), heading.ordinal >= 0 else { throw WriteError.malformed("heading level or ordinal out of range") }
        }
        if case .after(_, let anchorOrdinal) = place, anchorOrdinal < 0 {
            throw WriteError.malformed("negative ordinal")
        }
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
        case .moveBlock:
            guard lineHash != nil else { throw WriteError.malformed("move_block incomplete") }
        case .removeBlock:
            guard lineHash != nil else { throw WriteError.malformed("remove_block incomplete") }
        case .moveFile:
            guard let toPath, !toPath.trimmingCharacters(in: .whitespaces).isEmpty else { throw WriteError.malformed("move_file without a destination") }
            guard toPath != path else { throw WriteError.malformed("move_file onto itself") }
        case .putFile:
            guard let contentBase64, !contentBase64.trimmingCharacters(in: .whitespaces).isEmpty,
                  let contentHash, !contentHash.trimmingCharacters(in: .whitespaces).isEmpty
            else { throw WriteError.malformed("put_file without bytes or a hash") }
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
        case to
        case place
        case createUnder = "create_under"
        case contentBase64 = "content_base64"
        case contentHash = "content_hash"
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        version = try container.decode(Int.self, forKey: .version)
        clientID = try container.decode(String.self, forKey: .clientID)
        kind = try container.decode(WriteKind.self, forKey: .kind)
        path = try container.decode(String.self, forKey: .path)
        madeAt = try container.decodeIfPresent(String.self, forKey: .madeAt) ?? ""
        deviceID = try container.decodeIfPresent(String.self, forKey: .deviceID) ?? ""
        content = try container.decodeIfPresent(String.self, forKey: .content)
        heading = try container.decodeIfPresent(HeadingRef.self, forKey: .heading)
        // Required where the kind carries lines, as the desk's parser has it.
        if kind == .append || kind == .replaceSection {
            lines = try container.decode([String].self, forKey: .lines)
        } else {
            lines = try container.decodeIfPresent([String].self, forKey: .lines) ?? []
        }
        placement = try container.decodeIfPresent(Placement.self, forKey: .placement) ?? .end
        blankLineBefore = try container.decodeIfPresent(Bool.self, forKey: .blankLineBefore) ?? false
        createFromTemplate = try container.decodeIfPresent(Bool.self, forKey: .createFromTemplate) ?? false
        lineHash = try container.decodeIfPresent(String.self, forKey: .lineHash)
        ordinal = try container.decodeIfPresent(Int.self, forKey: .ordinal) ?? 0
        newLine = try container.decodeIfPresent(String.self, forKey: .newLine)
        baseHash = try container.decodeIfPresent(String.self, forKey: .baseHash)
        if kind == .moveFile {
            toPath = try container.decodeIfPresent(String.self, forKey: .to)
        } else {
            to = try container.decodeIfPresent(HeadingRef.self, forKey: .to)
        }
        place = try container.decodeIfPresent(Place.self, forKey: .place) ?? .end
        createUnder = try container.decodeIfPresent(HeadingRef.self, forKey: .createUnder)
        contentBase64 = try container.decodeIfPresent(String.self, forKey: .contentBase64)
        contentHash = try container.decodeIfPresent(String.self, forKey: .contentHash)
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
        case .moveBlock:
            try container.encode(heading, forKey: .heading)
            try container.encode(lineHash, forKey: .lineHash)
            try container.encode(ordinal, forKey: .ordinal)
            try container.encode(to, forKey: .to)
            try container.encode(place, forKey: .place)
            try container.encode(newLine, forKey: .newLine)
            try container.encode(createUnder, forKey: .createUnder)
        case .removeBlock:
            try container.encode(heading, forKey: .heading)
            try container.encode(lineHash, forKey: .lineHash)
            try container.encode(ordinal, forKey: .ordinal)
        case .moveFile:
            try container.encode(toPath, forKey: .to)
        case .putFile:
            try container.encode(contentBase64, forKey: .contentBase64)
            try container.encode(contentHash, forKey: .contentHash)
        }
    }
}
