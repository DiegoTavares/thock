import Foundation

public struct PendingWrite: Equatable, Sendable {
    public var localID: Int64
    public var document: WriteDocument
    public var baseVersion: Int
    public var seed: SeedInfo?
    /// Set once the server has accepted the write.
    public var seq: Int?
}

/// The phone's copy of the vault: decrypted notes, the queue of writes the
/// desk has not applied yet, and the record of captures (V34 §6.3). One
/// SQLite file in the app group container, shared by the app, the share
/// sheet and the widgets.
public final class VaultStore: @unchecked Sendable {
    public static let didChange = Notification.Name("ThockVaultStoreDidChange")

    private let database: Database
    private let lock = NSRecursiveLock()

    /// `nil` keeps the store in memory, for tests and previews.
    public init(url: URL?) throws {
        database = try Database(path: url?.path ?? ":memory:")
        try database.execute("""
            CREATE TABLE IF NOT EXISTS files (
                path TEXT PRIMARY KEY,
                version INTEGER NOT NULL DEFAULT 0,
                snapshot TEXT,
                content TEXT NOT NULL,
                content_hash TEXT,
                blob_id TEXT
            )
            """)
        try database.execute("""
            CREATE TABLE IF NOT EXISTS pending_writes (
                local_id INTEGER PRIMARY KEY AUTOINCREMENT,
                client_id TEXT NOT NULL UNIQUE,
                path TEXT NOT NULL,
                base_version INTEGER NOT NULL,
                json TEXT NOT NULL,
                seed TEXT,
                seq INTEGER
            )
            """)
        try database.execute("""
            CREATE TABLE IF NOT EXISTS captures (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                digest TEXT NOT NULL,
                title TEXT NOT NULL,
                kind TEXT NOT NULL,
                destination TEXT NOT NULL,
                made_at INTEGER NOT NULL,
                inbox_path TEXT
            )
            """)
        try database.execute("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        if let url {
            // Background refresh has to read the store while the phone is
            // locked, so the class is "until first unlock" (V34 §6.3).
            try? FileManager.default.setAttributes([.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication], ofItemAtPath: url.path)
        }
    }

    private func locked<T>(_ body: () throws -> T) rethrows -> T {
        lock.lock()
        defer { lock.unlock() }
        return try body()
    }

    private func changed() {
        NotificationCenter.default.post(name: Self.didChange, object: self)
    }

    // MARK: Notes

    public func content(_ path: String) -> String? {
        locked {
            (try? database.query("SELECT content FROM files WHERE path = ?", [.text(path)]))?.first?.first?.string
        }
    }

    public func exists(_ path: String) -> Bool {
        content(path) != nil
    }

    public func version(_ path: String) -> Int {
        locked {
            Int((try? database.query("SELECT version FROM files WHERE path = ?", [.text(path)]))?.first?.first?.int ?? 0)
        }
    }

    public func contentHash(_ path: String) -> String? {
        locked {
            (try? database.query("SELECT content_hash FROM files WHERE path = ?", [.text(path)]))?.first?.first?.string
        }
    }

    public func paths(under directory: String? = nil) -> [String] {
        locked {
            let rows: [[Database.Value]]?
            if let directory {
                // `substr` rather than LIKE, so a folder named `50%_done`
                // cannot turn into a wildcard.
                let prefix = directory + "/"
                rows = try? database.query("SELECT path FROM files WHERE substr(path, 1, ?) = ? ORDER BY path", [.int(Int64(prefix.count)), .text(prefix)])
            } else {
                rows = try? database.query("SELECT path FROM files ORDER BY path")
            }
            return rows?.compactMap { $0.first?.string } ?? []
        }
    }

    public var config: VaultConfig {
        VaultConfig(config: content(VaultConfig.configPath), inboxConfig: content(VaultConfig.inboxConfigPath))
    }

    /// The expanded template a note would be created from, when the vault has
    /// one. The desk expands the same file with the same tokens.
    public func seedText(_ seed: SeedInfo?) -> String? {
        guard let seed else { return nil }
        let config = config
        let notes = seed.kind == .daily ? config.daily : config.weekly
        guard let template = content(notes.template) else { return nil }
        return Template.expand(template, day: seed.day, time: seed.time, title: seed.day.formatted(notes.filename))
    }

    // MARK: Writes

    /// Applies each write to the local copy and queues it for the desk. A
    /// write whose effect is already there is dropped instead of queued.
    @discardableResult
    public func record(_ writes: [PlannedWrite]) throws -> [Outcome] {
        let outcomes: [Outcome] = try locked {
            var outcomes: [Outcome] = []
            try database.execute("BEGIN IMMEDIATE")
            do {
                for planned in writes {
                    let document = planned.document
                    let existing = content(document.path)
                    let applied = SyncCore.apply(existing: existing, write: document, seed: seedText(planned.seed))
                    outcomes.append(applied.outcome)
                    guard applied.outcome != .noop else { continue }
                    let version = version(document.path)
                    try database.execute(
                        "INSERT INTO files (path, version, content) VALUES (?, 0, ?) ON CONFLICT(path) DO UPDATE SET content = excluded.content",
                        [.text(document.path), .text(applied.text)])
                    let seed = planned.seed.flatMap { try? JSONEncoder().encode($0) }.flatMap { String(data: $0, encoding: .utf8) }
                    try database.execute(
                        "INSERT INTO pending_writes (client_id, path, base_version, json, seed) VALUES (?, ?, ?, ?, ?)",
                        [.text(document.clientID), .text(document.path), .int(Int64(version)), .text(document.json()), seed.map { .text($0) } ?? .null])
                }
                try database.execute("COMMIT")
            } catch {
                try? database.execute("ROLLBACK")
                throw error
            }
            return outcomes
        }
        changed()
        return outcomes
    }

    public func pending(path: String? = nil) -> [PendingWrite] {
        locked {
            let rows: [[Database.Value]]?
            if let path {
                rows = try? database.query("SELECT local_id, json, base_version, seed, seq FROM pending_writes WHERE path = ? ORDER BY local_id", [.text(path)])
            } else {
                rows = try? database.query("SELECT local_id, json, base_version, seed, seq FROM pending_writes ORDER BY local_id")
            }
            return (rows ?? []).compactMap { row in
                guard let json = row[1].string, let document = try? WriteDocument.parse(json) else { return nil }
                let seed = row[3].string.flatMap { try? JSONDecoder().decode(SeedInfo.self, from: Data($0.utf8)) }
                return PendingWrite(localID: row[0].int ?? 0, document: document, baseVersion: Int(row[2].int ?? 0), seed: seed, seq: row[4].int.map(Int.init))
            }
        }
    }

    public var waitingForDeskCount: Int {
        pending().count
    }

    public func markSent(clientID: String, seq: Int) {
        locked {
            try? database.execute("UPDATE pending_writes SET seq = ? WHERE client_id = ?", [.int(Int64(seq)), .text(clientID)])
        }
    }

    /// Drops a write the service will never accept and rebuilds its note
    /// from the desk's copy and the writes still waiting.
    public func discard(clientID: String) throws {
        try locked {
            guard let path = try database.query("SELECT path FROM pending_writes WHERE client_id = ?", [.text(clientID)]).first?.first?.string else { return }
            try database.execute("DELETE FROM pending_writes WHERE client_id = ?", [.text(clientID)])
            let row = try database.query("SELECT version, snapshot, content_hash, blob_id FROM files WHERE path = ?", [.text(path)]).first
            try store(path: path, version: Int(row?[0].int ?? 0), snapshot: row?[1].string, contentHash: row?[2].string, blobID: row?[3].string)
        }
        changed()
    }

    // MARK: From the desk

    private func rebased(snapshot: String?, path: String) -> String? {
        var content = snapshot
        for write in pending(path: path) {
            let applied = SyncCore.apply(existing: content, write: write.document, seed: seedText(write.seed))
            if content == nil, applied.outcome == .noop { continue }
            content = applied.text
        }
        return content
    }

    private func store(path: String, version: Int, snapshot: String?, contentHash: String?, blobID: String?) throws {
        guard let content = rebased(snapshot: snapshot, path: path) else {
            try database.execute("DELETE FROM files WHERE path = ?", [.text(path)])
            return
        }
        try database.execute(
            """
            INSERT INTO files (path, version, snapshot, content, content_hash, blob_id) VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(path) DO UPDATE SET version = excluded.version, snapshot = excluded.snapshot,
                content = excluded.content, content_hash = excluded.content_hash, blob_id = excluded.blob_id
            """,
            [.text(path), .int(Int64(version)), snapshot.map { .text($0) } ?? .null, .text(content),
             contentHash.map { .text($0) } ?? .null, blobID.map { .text($0) } ?? .null])
    }

    /// A snapshot from the desk replaces the local copy, then every write
    /// still waiting for that note is applied on top again (V34 API §8.4).
    public func applySnapshot(path: String, version: Int, content: String, contentHash: String, blobID: String) throws {
        try locked {
            try store(path: path, version: version, snapshot: content, contentHash: contentHash, blobID: blobID)
        }
        changed()
    }

    public func applyTombstone(path: String, version: Int) throws {
        try locked {
            try store(path: path, version: version, snapshot: nil, contentHash: nil, blobID: nil)
        }
        changed()
    }

    /// After a full pull: anything the desk no longer has goes, unless a
    /// write is still waiting on it.
    public func removeFiles(notIn listed: Set<String>) throws {
        try locked {
            let waiting = Set(pending().map(\.document.path))
            for path in paths() where !listed.contains(path) {
                if waiting.contains(path) {
                    try store(path: path, version: 0, snapshot: nil, contentHash: nil, blobID: nil)
                } else {
                    try database.execute("DELETE FROM files WHERE path = ?", [.text(path)])
                }
            }
        }
        changed()
    }

    /// Drops the writes the desk has applied, once the phone has pulled past
    /// the version the desk acked at. What the desk made of each write is the
    /// final word, so the local copy is rebuilt from its snapshot.
    public func prune(ackedThroughSeq: Int, ackedAtVersion: Int) throws {
        guard cursor >= ackedAtVersion else { return }
        let missing = refetchPaths
        let touched: [String] = try locked {
            let rows = try database.query("SELECT DISTINCT path FROM pending_writes WHERE seq IS NOT NULL AND seq <= ?", [.int(Int64(ackedThroughSeq))])
            // A note whose snapshot did not arrive keeps its writes: dropping
            // them would show it without the phone's own change until then.
            let paths = rows.compactMap { $0.first?.string }.filter { !missing.contains($0) }
            guard !paths.isEmpty else { return [] }
            for path in paths {
                try database.execute("DELETE FROM pending_writes WHERE seq IS NOT NULL AND seq <= ? AND path = ?", [.int(Int64(ackedThroughSeq)), .text(path)])
            }
            for path in paths {
                let row = try database.query("SELECT version, snapshot, content_hash, blob_id FROM files WHERE path = ?", [.text(path)]).first
                try store(path: path, version: Int(row?[0].int ?? 0), snapshot: row?[1].string, contentHash: row?[2].string, blobID: row?[3].string)
            }
            return paths
        }
        if !touched.isEmpty {
            changed()
        }
    }

    // MARK: Meta

    public func meta(_ key: String) -> String? {
        locked {
            (try? database.query("SELECT value FROM meta WHERE key = ?", [.text(key)]))?.first?.first?.string
        }
    }

    public func setMeta(_ key: String, _ value: String?) {
        locked {
            if let value {
                try? database.execute("INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [.text(key), .text(value)])
            } else {
                try? database.execute("DELETE FROM meta WHERE key = ?", [.text(key)])
            }
        }
    }

    /// The highest version the phone has fully processed.
    public var cursor: Int {
        get { Int(meta("cursor") ?? "") ?? 0 }
        set { setMeta("cursor", String(newValue)) }
    }

    /// Notes whose latest snapshot could not be taken. The cursor moves past
    /// them so one bad version cannot hold every other note back; they are
    /// fetched again by a full pull, and their writes are kept until then.
    public var refetchPaths: Set<String> {
        get { Set((meta("refetch") ?? "").split(separator: "\n").map(String.init)) }
        set { setMeta("refetch", newValue.isEmpty ? nil : newValue.sorted().joined(separator: "\n")) }
    }

    public var deviceID: String {
        meta("device_id") ?? "unpaired"
    }

    public var isConnected: Bool {
        meta("vault_id") != nil
    }

    /// Plus lapsed: the phone keeps showing what it has and stops writing.
    public var isReadOnly: Bool {
        meta("status") == "lapsed"
    }

    // MARK: Captures

    public func addCapture(_ record: CaptureRecord) {
        locked {
            try? database.execute(
                "INSERT INTO captures (digest, title, kind, destination, made_at, inbox_path) VALUES (?, ?, ?, ?, ?, ?)",
                [.text(record.digest), .text(record.title), .text(record.kind.rawValue), .text(record.destination.rawValue),
                 .int(Int64(record.madeAt.timeIntervalSince1970 * 1000)), record.inboxPath.map { .text($0) } ?? .null])
        }
        changed()
    }

    public func renameCapture(inboxPath: String, title: String) {
        locked {
            try? database.execute("UPDATE captures SET title = ? WHERE inbox_path = ?", [.text(title), .text(inboxPath)])
        }
        changed()
    }

    public func captures() -> [CaptureRecord] {
        locked {
            let rows = (try? database.query("SELECT id, digest, title, kind, destination, made_at, inbox_path FROM captures ORDER BY made_at DESC, id DESC")) ?? []
            return rows.compactMap { row in
                guard let digest = row[1].string, let title = row[2].string,
                      let kind = row[3].string.flatMap(CaptureKind.init(rawValue:)),
                      let destination = row[4].string.flatMap(CaptureDestination.init(rawValue:))
                else { return nil }
                let seconds = Double(row[5].int ?? 0) / 1000
                return CaptureRecord(id: row[0].int ?? 0, digest: digest, title: title, kind: kind, destination: destination,
                                     madeAt: Date(timeIntervalSince1970: seconds), inboxPath: row[6].string)
            }
        }
    }

    public func receipts() -> [(record: CaptureRecord, state: ReceiptState)] {
        let log = TriageLog.parse(content(VaultConfig.triageLogPath) ?? "")
        return captures().map { ($0, Receipts.state(of: $0, exists: exists, log: log)) }
    }

    /// Inbox notes still waiting for triage, whoever captured them.
    public func waitingInboxNotes() -> [InboxNote] {
        let directory = config.inboxDir
        return paths(under: directory)
            .filter { $0.hasSuffix(".md") && !$0.dropFirst(directory.count + 1).contains("/") }
            .compactMap { path in content(path).map { InboxNote(path: path, content: $0) } }
    }

    /// Forgets the vault: used when the desk disconnects this phone.
    public func wipe() {
        locked {
            for table in ["files", "pending_writes", "captures", "meta"] {
                try? database.execute("DELETE FROM \(table)")
            }
        }
        changed()
    }
}
