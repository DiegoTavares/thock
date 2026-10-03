import Foundation

/// What a tool call came to: the text the model reads, the line the person
/// sees while it runs, and the note it opened, when it opened one.
public struct ToolOutcome: Equatable, Sendable {
    public var result: String
    public var activity: String
    public var source: String?
}

/// The agent's four tools on the phone (V35 §5.3), served from the local
/// copy of the vault. There is nothing here that reaches the network or
/// changes a note the person wrote.
public struct AskTools: Sendable {
    static let searchLimit = 12
    static let readLines = 400
    static let readCharacters = 30_000
    static let listLimit = 200

    public let session: VaultSession
    public var now: Date

    public init(session: VaultSession, now: Date = Date()) {
        self.session = session
        self.now = now
    }

    static func tool(_ name: String, _ description: String, properties: [String: [String: String]], required: [String]) -> [String: Any] {
        [
            "type": "function",
            "function": [
                "name": name,
                "description": description,
                "parameters": ["type": "object", "properties": properties, "required": required] as [String: Any],
            ] as [String: Any],
        ]
    }

    /// The definitions sent with every model call, in the gateway's format.
    public static var definitions: [[String: Any]] {
        [
            tool("search", "Find notes by the words in them, best matches first. Returns each note's path and an excerpt.",
                 properties: [
                    "query": ["type": "string", "description": "A few distinctive words. Any of them may match; a word also matches longer words that start with it."],
                    "folder": ["type": "string", "description": "Only look under this folder, for example daily."],
                 ], required: ["query"]),
            tool("read", "Open a note and return its text.",
                 properties: [
                    "path": ["type": "string", "description": "The note's path inside the vault, for example daily/2026-09-12.md."],
                    "from_line": ["type": "integer", "description": "Start at this line, to continue a long note."],
                 ], required: ["path"]),
            tool("list", "Show the folders and notes inside a folder. Leave folder out for the top of the vault.",
                 properties: [
                    "folder": ["type": "string", "description": "A folder inside the vault, for example daily."],
                 ], required: []),
            tool("append", "Add a line to memory/inbox.md, for something about this person worth remembering. No other note can be changed from the phone.",
                 properties: [
                    "path": ["type": "string", "description": "Always memory/inbox.md."],
                    "text": ["type": "string", "description": "One line: - YYYY-MM-DD · the fact, in your words."],
                 ], required: ["path", "text"]),
        ]
    }

    static func normalized(_ path: String) -> String {
        VaultConfig.normalizedFolder(path.trimmingCharacters(in: .whitespaces))
    }

    public func run(name: String, arguments: String) -> ToolOutcome {
        let parsed = (try? JSONSerialization.jsonObject(with: Data(arguments.utf8))) as? [String: Any]
        guard let parsed else {
            return ToolOutcome(result: "That request could not be read. Send the arguments as one JSON object.", activity: "Looking through your notes")
        }
        func text(_ key: String) -> String {
            (parsed[key] as? String ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        }
        switch name {
        case "search":
            return search(query: text("query"), folder: text("folder"))
        case "read":
            let line = parsed["from_line"] as? Int ?? (parsed["from_line"] as? Double).map(Int.init) ?? 1
            return read(path: text("path"), fromLine: line)
        case "list":
            return list(folder: text("folder"))
        case "append":
            return append(path: text("path"), text: text("text"))
        default:
            return ToolOutcome(result: "There is no tool called \(name). The tools are search, read, list and append.", activity: "Looking through your notes")
        }
    }

    func search(query: String, folder: String) -> ToolOutcome {
        let activity = "Looking through your notes for \u{201C}\(query)\u{201D}"
        guard !query.isEmpty else {
            return ToolOutcome(result: "Give a few words to search for.", activity: "Looking through your notes")
        }
        let hits = session.store.search(query, folder: folder.isEmpty ? nil : folder, limit: Self.searchLimit)
        guard !hits.isEmpty else {
            return ToolOutcome(result: "No note matches those words. Try other words, or list a folder.", activity: activity)
        }
        let lines = hits.map { "\($0.path): \($0.excerpt)" }
        return ToolOutcome(result: lines.joined(separator: "\n"), activity: activity)
    }

    func read(path: String, fromLine: Int) -> ToolOutcome {
        let path = Self.normalized(path)
        let activity = "Reading \(path)"
        guard let content = session.store.content(path) else {
            return ToolOutcome(result: "There is no note at \(path). Search or list a folder to find the right path.", activity: activity)
        }
        let lines = content.components(separatedBy: "\n")
        let start = min(max(fromLine, 1), max(lines.count, 1)) - 1
        var taken: [String] = []
        var characters = 0
        for line in lines[start...] {
            guard taken.count < Self.readLines, characters + line.count <= Self.readCharacters || taken.isEmpty else { break }
            taken.append(line)
            characters += line.count + 1
        }
        var result = taken.joined(separator: "\n")
        let next = start + taken.count
        if next < lines.count {
            result += "\n\n[\(lines.count - next) more lines. Read again with from_line \(next + 1) to continue.]"
        }
        return ToolOutcome(result: result, activity: activity, source: path)
    }

    func list(folder: String) -> ToolOutcome {
        let folder = Self.normalized(folder)
        let activity = folder.isEmpty ? "Looking at your folders" : "Looking in \(folder)"
        let prefix = folder.isEmpty ? "" : folder + "/"
        // The app's own settings are not notes.
        let paths = session.store.paths(under: folder.isEmpty ? nil : folder).filter { !$0.hasPrefix(".thock/") }
        guard !paths.isEmpty else {
            return ToolOutcome(result: folder.isEmpty ? "The vault is empty." : "There is nothing under \(folder).", activity: activity)
        }
        var folders: [String: Int] = [:]
        var notes: [String] = []
        for path in paths {
            let rest = path.dropFirst(prefix.count)
            if let slash = rest.firstIndex(of: "/") {
                folders[String(rest[..<slash]), default: 0] += 1
            } else {
                notes.append(path)
            }
        }
        var lines = folders.keys.sorted().map { "\(prefix)\($0)/ (\(folders[$0] ?? 0) notes)" }
        if notes.count > Self.listLimit {
            // Dated names sort oldest first, so the newest are the ones kept.
            lines.append("[\(notes.count - Self.listLimit) earlier notes not shown, from \(notes[0])]")
            notes = Array(notes.suffix(Self.listLimit))
        }
        lines += notes
        return ToolOutcome(result: lines.joined(separator: "\n"), activity: activity)
    }

    func append(path: String, text: String) -> ToolOutcome {
        guard Self.normalized(path) == PhoneWrites.memoryInboxPath else {
            return ToolOutcome(
                result: "From the phone you can only add a line to memory/inbox.md. Nothing was written. Tell the person the capture button on their phone adds tasks, ideas and journal entries, and that other changes wait for the desk.",
                activity: "Looking through your notes")
        }
        do {
            guard try session.remember(text, now: now) else {
                return ToolOutcome(result: "There was nothing to add.", activity: "Looking through your notes")
            }
            return ToolOutcome(result: "Added to memory/inbox.md.", activity: "Noted one thing for later")
        } catch {
            return ToolOutcome(result: "The line could not be added right now. Carry on without it.", activity: "Looking through your notes")
        }
    }
}
