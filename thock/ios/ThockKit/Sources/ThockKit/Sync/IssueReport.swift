import Foundation

/// What *Report a problem* puts in the email: the facts of this phone's
/// connection, enough to say why notes are not arriving, and never a note.
/// The person reads it in their mail app before it goes anywhere.
public struct IssueReport: Equatable, Sendable {
    public static let subject = "A problem with Thock on my iPhone"
    /// Failures past this many are counted, not listed.
    static let failuresShown = 10

    public var appVersion: String
    public var build: String
    /// The OS and the hardware, as in `iOS 18.1 · iPhone15,2`.
    public var system: String
    public var phoneName: String?
    public var address: String?
    public var isPractice: Bool
    public var isConnected: Bool
    public var state: SyncState
    public var waitingForDesk: Int
    public var notesHere: Int
    public var diagnostics: SyncDiagnostics

    public init(appVersion: String, build: String, system: String, phoneName: String?, address: String?, isPractice: Bool, isConnected: Bool,
                state: SyncState, waitingForDesk: Int, notesHere: Int, diagnostics: SyncDiagnostics) {
        self.appVersion = appVersion
        self.build = build
        self.system = system
        self.phoneName = phoneName
        self.address = address
        self.isPractice = isPractice
        self.isConnected = isConnected
        self.state = state
        self.waitingForDesk = waitingForDesk
        self.notesHere = notesHere
        self.diagnostics = diagnostics
    }

    /// The email's text: room to describe the problem, then the details.
    public var body: String {
        var lines = [
            "Tell us what happened, and what you expected instead:",
            "",
            "",
            "",
            "— Details Thock added —",
            "Thock for iPhone \(appVersion) (\(build)) · \(system)",
            "Connection: \(connection)",
            "Status: \(Self.word(for: state))" + (waitingForDesk > 0 ? " · \(waitingForDesk) \(waitingForDesk == 1 ? "change" : "changes") waiting for the desk" : ""),
        ]
        if isConnected {
            let d = diagnostics
            lines.append("Notes here: \(notesHere) · at the desk's copy: \(d.serverFileCount.map(String.init) ?? "?")")
            lines.append("Version \(d.cursor) of \(d.serverLatestVersion.map(String.init) ?? "?") · writes not yet at the desk: \(d.serverPendingWrites.map(String.init) ?? "?")")
            if let address {
                lines.append("Address: \(address)")
            }
            lines.append("Last check: " + (d.lastRound.map(Self.stamp.string(from:)) ?? "never"))
            if let error = d.lastError {
                lines.append("Problem: " + error)
            }
            for failure in d.failures.prefix(Self.failuresShown) {
                lines.append("Could not take: " + failure)
            }
            if d.failures.count > Self.failuresShown {
                lines.append("and \(d.failures.count - Self.failuresShown) more")
            }
        }
        return lines.joined(separator: "\n")
    }

    /// A `mailto:` link that opens the mail app on the report, or nil when
    /// the address cannot be made into one.
    public func mailURL(to address: String) -> URL? {
        guard let recipient = Self.encode(address, keeping: "@"), let subject = Self.encode(Self.subject),
              let body = Self.encode(body.replacingOccurrences(of: "\n", with: "\r\n")) else { return nil }
        return URL(string: "mailto:\(recipient)?subject=\(subject)&body=\(body)")
    }

    private var connection: String {
        if isPractice { return "practice notebook" }
        guard isConnected else { return "not connected" }
        return "connected" + (phoneName.map { " as \($0)" } ?? "")
    }

    static func word(for state: SyncState) -> String {
        switch state {
        case .notConnected: return "not connected"
        case .upToDate: return "up to date"
        case .working: return "checking with the desk"
        case .offline: return "can't reach the desk's copy"
        case .paused: return "paused, Thock Plus has ended"
        case .disconnected: return "disconnected by the desk"
        }
    }

    private static let stamp: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter
    }()

    /// Mail apps read `+` and `&` literally, so everything outside the
    /// unreserved set is percent-encoded.
    private static func encode(_ text: String, keeping extra: String = "") -> String? {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~" + extra)
        return text.addingPercentEncoding(withAllowedCharacters: allowed)
    }
}
