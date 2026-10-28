import Foundation

/// The watch's own two files (V48 §5): the last snapshot the phone sent and
/// the requests it has not answered yet, with the capture being confirmed.
/// Both are JSON written atomically. Neither is a store of notes.
public struct WatchOutbox: Codable, Equatable, Sendable {
    public enum Request: Codable, Hashable, Sendable {
        case capture(WatchCapture)
        case tick(WatchTick)

        public var id: UUID {
            switch self {
            case .capture(let capture): return capture.id
            case .tick(let tick): return tick.id
            }
        }

        public var message: WatchMessage {
            switch self {
            case .capture(let capture): return .capture(capture)
            case .tick(let tick): return .tick(tick)
            }
        }

        /// The same request under a new id: what *Try again* sends, since
        /// the phone answers an id it has seen as it answered it then.
        func renewed() -> Request {
            switch self {
            case .capture(var capture):
                capture.id = UUID()
                return .capture(capture)
            case .tick(var tick):
                tick.id = UUID()
                return .tick(tick)
            }
        }
    }

    public enum State: Codable, Hashable, Sendable {
        /// Not answered yet.
        case waiting
        /// The phone said no, in these words.
        case refused(String)
        /// The phone saved it; kept only until a snapshot shows it.
        case saved(Date)
    }

    public struct Item: Codable, Hashable, Identifiable, Sendable {
        public var request: Request
        public var state: State
        /// Handed to `transferUserInfo`, which the system delivers once, across
        /// restarts: it is never handed over again.
        public var transferred: Bool

        public var id: UUID { request.id }

        public init(request: Request, state: State = .waiting, transferred: Bool = false) {
            self.request = request
            self.state = state
            self.transferred = transferred
        }
    }

    /// The capture on the confirm or *Remind me* screen (§8.5): written
    /// nowhere until it is saved, and never lost by lowering the wrist.
    public struct Draft: Codable, Hashable, Sendable {
        public var text: String
        public var chip: CaptureDestination
        /// The chip *Remind me* returns to when it closes without a time.
        public var before: CaptureDestination
        public var due: String?

        public init(text: String, chip: CaptureDestination, before: CaptureDestination? = nil, due: ReminderDue? = nil) {
            self.text = text
            self.chip = chip
            self.before = before ?? (chip.isRemembered ? chip : .today)
            self.due = due?.description
        }

        public var reminderDue: ReminderDue? { due.flatMap(ReminderDue.init) }
    }

    public var items: [Item] = []
    public var draft: Draft?
    /// The chip the watch's last capture was saved with, never Reminder.
    public var chip: CaptureDestination?

    public init() {}

    /// What the outbox row and screen list: unanswered and refused requests.
    public var unsent: [Item] {
        items.filter { item in
            if case .saved = item.state { return false }
            return true
        }
    }

    public var waitingCount: Int { unsent.count }

    public mutating func add(_ request: Request) {
        items.append(Item(request: request))
    }

    /// Applies the phone's answer. A refused tick is dropped: the watch says
    /// why, and the line it named is gone. A refused capture stays, with the
    /// phone's words, until *Try again* or *Delete*.
    @discardableResult
    public mutating func apply(_ ack: WatchAck, at date: Date = Date()) -> Item? {
        guard let position = items.firstIndex(where: { $0.id == ack.id }) else { return nil }
        let item = items[position]
        switch ack.outcome {
        case .saved:
            items[position].state = .saved(date)
        case .refused:
            if case .tick = item.request {
                items.remove(at: position)
            } else {
                items[position].state = .refused(ack.message ?? VaultSession.failedMessage)
            }
        }
        return item
    }

    /// Marks a request unreadable by the phone's app: it waits, refused,
    /// until the phone is updated and the person tries again.
    public mutating func needsUpdate(_ id: UUID) {
        guard let position = items.firstIndex(where: { $0.id == id }) else { return }
        items[position].state = .refused(WatchOutbox.updateMessage)
    }

    public static let updateMessage = "Update Thock on your iPhone"

    /// *Try again*: a new id, waiting, sent afresh. Returns it.
    public mutating func retry(_ id: UUID) -> Item? {
        guard let position = items.firstIndex(where: { $0.id == id }) else { return nil }
        items[position] = Item(request: items[position].request.renewed())
        return items[position]
    }

    /// Takes back the transferred mark of every waiting request the system
    /// no longer holds for delivery: it was delivered and its answer is
    /// late, or it was lost. Either way it may go again, since the phone
    /// answers a repeat without writing. Returns whether anything changed.
    @discardableResult
    public mutating func reconcile(outstanding: Set<UUID>) -> Bool {
        var changed = false
        for position in items.indices where items[position].transferred && items[position].state == .waiting && !outstanding.contains(items[position].id) {
            items[position].transferred = false
            changed = true
        }
        return changed
    }

    public mutating func delete(_ id: UUID) {
        items.removeAll { $0.id == id }
    }

    /// Saved requests a snapshot made after them already shows.
    public mutating func forgetSaved(before snapshot: Date) {
        items.removeAll { item in
            if case .saved(let at) = item.state { return at <= snapshot }
            return false
        }
    }

    /// Whether a tick sent from here, not refused, still stands for a row
    /// the snapshot may not show done yet.
    public func isTicked(day: String, hash: String, ordinal: Int) -> Bool {
        items.contains { item in
            guard case .tick(let tick) = item.request, tick.day == day, tick.hash == hash, tick.ordinal == ordinal else { return false }
            if case .refused = item.state { return false }
            return true
        }
    }
}

/// Reads and writes the watch's two files in a folder: the watch app's and
/// its widget's shared container.
public struct WatchFiles: Sendable {
    public var folder: URL

    public init(folder: URL) {
        self.folder = folder
    }

    var snapshotURL: URL { folder.appendingPathComponent("snapshot.json") }
    var outboxURL: URL { folder.appendingPathComponent("outbox.json") }

    /// The last snapshot, or `nil` when none has arrived. A file this app
    /// cannot read is treated as none: the next send replaces it.
    public func snapshot() -> WatchSnapshot? {
        guard let data = try? Data(contentsOf: snapshotURL), case .snapshot(let snapshot)? = try? WatchMessage(data: data) else { return nil }
        return snapshot
    }

    public func save(_ snapshot: WatchSnapshot) throws {
        try WatchMessage.snapshot(snapshot).encoded().write(to: snapshotURL, options: .atomic)
    }

    public func outbox() throws -> WatchOutbox {
        guard FileManager.default.fileExists(atPath: outboxURL.path) else { return WatchOutbox() }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(WatchOutbox.self, from: Data(contentsOf: outboxURL))
    }

    public func save(_ outbox: WatchOutbox) throws {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        try encoder.encode(outbox).write(to: outboxURL, options: .atomic)
    }
}
