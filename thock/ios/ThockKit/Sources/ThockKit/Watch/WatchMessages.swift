import Foundation

/// The private contract between the phone and its watch (V48 §6). It never
/// crosses the network, so it is not part of V34; both apps compile these
/// types from here.
public enum WatchLink {
    public static let version = 1
}

public enum WatchLinkError: Error, Equatable {
    /// A message from a newer or older app. Ignored, never retried.
    case unknownVersion(Int?)
    case malformed
}

/// One line of a day as the watch draws it (§6.1): resolved on the phone, so
/// the watch never reads Markdown.
public struct WatchRow: Codable, Hashable, Sendable {
    public enum Kind: String, Codable, Sendable {
        case task
        case calendar
        case reminder
    }

    /// The line's identity for a tick.
    public var hash: String
    public var ordinal: Int
    public var kind: Kind
    /// Minutes after midnight; `nil` when untimed.
    public var start: Int?
    public var end: Int?
    public var label: String
    public var done: Bool
    /// A reminder still open from an earlier day (`2026-10-27`).
    public var carriedFrom: String?

    public init(hash: String, ordinal: Int, kind: Kind, start: Int? = nil, end: Int? = nil, label: String, done: Bool = false, carriedFrom: String? = nil) {
        self.hash = hash
        self.ordinal = ordinal
        self.kind = kind
        self.start = start
        self.end = end
        self.label = label
        self.done = done
        self.carriedFrom = carriedFrom
    }

    enum CodingKeys: String, CodingKey {
        case hash, ordinal, kind, start, end, label, done
        case carriedFrom = "carried_from"
    }
}

public struct WatchDay: Codable, Hashable, Sendable {
    /// `2026-10-28`.
    public var day: String
    /// False when the day has no note yet: its rows are reminders only.
    public var exists: Bool
    public var rows: [WatchRow]
    /// Lines in the planner that are not checkboxes, shown as a count.
    public var notes: Int

    public init(day: String, exists: Bool, rows: [WatchRow], notes: Int) {
        self.day = day
        self.exists = exists
        self.rows = rows
        self.notes = notes
    }

    public var vaultDay: VaultDay? { VaultDay(iso: day) }
}

/// The phone's reading of the days around today (§6.1).
public struct WatchSnapshot: Codable, Hashable, Sendable {
    public enum State: String, Codable, Sendable {
        case ready
        case notConnected = "not_connected"
        case readOnly = "read_only"
    }

    public struct Reminders: Codable, Hashable, Sendable {
        public var dateOnlyAt: String
        public var tomorrowAt: String
        public var index: String

        public init(dateOnlyAt: String, tomorrowAt: String, index: String) {
            self.dateOnlyAt = dateOnlyAt
            self.tomorrowAt = tomorrowAt
            self.index = index
        }

        public init(_ settings: ReminderSettings) {
            self.init(dateOnlyAt: settings.dateOnlyAt.description, tomorrowAt: settings.tomorrowAt.description, index: settings.index)
        }

        /// The settings the watch's hints and presets read, with the
        /// phone's defaults for anything it cannot read.
        public var settings: ReminderSettings {
            var settings = ReminderSettings()
            settings.index = index
            if let time = ClockTime(dateOnlyAt) { settings.dateOnlyAt = time }
            if let time = ClockTime(tomorrowAt) { settings.tomorrowAt = time }
            return settings
        }

        enum CodingKeys: String, CodingKey {
            case dateOnlyAt = "date_only_at"
            case tomorrowAt = "tomorrow_at"
            case index
        }
    }

    public var v = WatchLink.version
    public var madeAt: Date
    public var state: State
    /// The phone's today, not the watch's clock.
    public var today: String
    public var days: [WatchDay]
    public var reminders: Reminders
    /// The phone's remembered chip for the watch's entry.
    public var chip: CaptureDestination

    public init(madeAt: Date, state: State, today: String, days: [WatchDay], reminders: Reminders, chip: CaptureDestination) {
        self.madeAt = madeAt
        self.state = state
        self.today = today
        self.days = days
        self.reminders = reminders
        self.chip = chip
    }

    public func day(_ day: VaultDay) -> WatchDay? {
        days.first { $0.day == day.iso }
    }

    /// Whether `day` lies in the range the snapshot covers, held or not.
    public func covers(_ day: VaultDay) -> Bool {
        guard let first = days.first?.vaultDay, let last = days.last?.vaultDay else { return false }
        return first <= day && day <= last
    }

    enum CodingKeys: String, CodingKey {
        case v
        case madeAt = "made_at"
        case state, today, days, reminders, chip
    }
}

public struct WatchCapture: Codable, Hashable, Sendable {
    public var v = WatchLink.version
    public var id: UUID
    /// When it was saved on the watch: the capture's instant on the phone.
    public var at: Date
    public var text: String
    public var destination: CaptureDestination
    /// `ReminderDue.description`: `2026-10-29 09:00` or `2026-10-29`.
    public var due: String?

    public init(id: UUID = UUID(), at: Date, text: String, destination: CaptureDestination, due: ReminderDue? = nil) {
        self.id = id
        self.at = at
        self.text = text
        self.destination = destination
        self.due = due?.description
    }

    public var reminderDue: ReminderDue? { due.flatMap(ReminderDue.init) }
}

public struct WatchTick: Codable, Hashable, Sendable {
    public var v = WatchLink.version
    public var id: UUID
    public var day: String
    public var hash: String
    public var ordinal: Int

    public init(id: UUID = UUID(), day: String, hash: String, ordinal: Int) {
        self.id = id
        self.day = day
        self.hash = hash
        self.ordinal = ordinal
    }
}

/// A day outside the snapshot's range.
public struct WatchDayAsk: Codable, Hashable, Sendable {
    public var v = WatchLink.version
    public var id: UUID
    public var day: String

    public init(id: UUID = UUID(), day: String) {
        self.id = id
        self.day = day
    }
}

public struct WatchAck: Codable, Hashable, Sendable {
    public enum Outcome: String, Codable, Sendable {
        case saved
        case refused
    }

    public var v = WatchLink.version
    public var id: UUID
    public var outcome: Outcome
    /// Only when refused: the sentence the watch shows.
    public var message: String?

    public init(id: UUID, outcome: Outcome, message: String? = nil) {
        self.id = id
        self.outcome = outcome
        self.message = message
    }
}

public struct WatchDayReply: Codable, Hashable, Sendable {
    public var v = WatchLink.version
    public var id: UUID
    public var day: WatchDay

    public init(id: UUID, day: WatchDay) {
        self.id = id
        self.day = day
    }
}

/// Every message either side sends, in the property-list dictionary form
/// WatchConnectivity carries.
public enum WatchMessage: Hashable, Sendable {
    case snapshot(WatchSnapshot)
    case capture(WatchCapture)
    case tick(WatchTick)
    case dayAsk(WatchDayAsk)
    case ack(WatchAck)
    case dayReply(WatchDayReply)

    static let typeKey = "type"

    var type: String {
        switch self {
        case .snapshot: return "snapshot"
        case .capture: return "capture"
        case .tick: return "tick"
        case .dayAsk: return "day_ask"
        case .ack: return "ack"
        case .dayReply: return "day_reply"
        }
    }

    /// The id a request or an answer carries; `nil` for a snapshot.
    public var requestID: UUID? {
        switch self {
        case .snapshot: return nil
        case .capture(let capture): return capture.id
        case .tick(let tick): return tick.id
        case .dayAsk(let ask): return ask.id
        case .ack(let ack): return ack.id
        case .dayReply(let reply): return reply.id
        }
    }

    private static var encoder: JSONEncoder {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        return encoder
    }

    private static var decoder: JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return decoder
    }

    /// The JSON encoding, which the watch also keeps on disk.
    public func encoded() throws -> Data {
        let body: Data
        switch self {
        case .snapshot(let value): body = try Self.encoder.encode(value)
        case .capture(let value): body = try Self.encoder.encode(value)
        case .tick(let value): body = try Self.encoder.encode(value)
        case .dayAsk(let value): body = try Self.encoder.encode(value)
        case .ack(let value): body = try Self.encoder.encode(value)
        case .dayReply(let value): body = try Self.encoder.encode(value)
        }
        guard var object = try JSONSerialization.jsonObject(with: body) as? [String: Any] else { throw WatchLinkError.malformed }
        object[Self.typeKey] = type
        return try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
    }

    /// Strings, numbers, booleans, arrays and dictionaries only: a property
    /// list. Optional fields that are `nil` are left out, never `NSNull`.
    public func dictionary() throws -> [String: Any] {
        guard let object = try JSONSerialization.jsonObject(with: encoded()) as? [String: Any] else { throw WatchLinkError.malformed }
        return object
    }

    public init(dictionary: [String: Any]) throws {
        guard JSONSerialization.isValidJSONObject(dictionary) else { throw WatchLinkError.malformed }
        try self.init(data: JSONSerialization.data(withJSONObject: dictionary))
    }

    /// Reads a message, refusing one whose `v` this app does not speak.
    public init(data: Data) throws {
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else { throw WatchLinkError.malformed }
        let version = (object["v"] as? NSNumber)?.intValue
        guard version == WatchLink.version else { throw WatchLinkError.unknownVersion(version) }
        let decoder = Self.decoder
        do {
            switch object[Self.typeKey] as? String {
            case "snapshot": self = .snapshot(try decoder.decode(WatchSnapshot.self, from: data))
            case "capture": self = .capture(try decoder.decode(WatchCapture.self, from: data))
            case "tick": self = .tick(try decoder.decode(WatchTick.self, from: data))
            case "day_ask": self = .dayAsk(try decoder.decode(WatchDayAsk.self, from: data))
            case "ack": self = .ack(try decoder.decode(WatchAck.self, from: data))
            case "day_reply": self = .dayReply(try decoder.decode(WatchDayReply.self, from: data))
            default: throw WatchLinkError.malformed
            }
        } catch let error as WatchLinkError {
            throw error
        } catch {
            throw WatchLinkError.malformed
        }
    }
}
