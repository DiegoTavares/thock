import Foundation

/// What *Report a problem* sends along with the person's words: the facts of
/// this phone's connection, enough to say why notes are not arriving, and
/// never a note. The person sees the details before sending (V41).
public struct IssueReport: Equatable, Sendable {
    /// Failures past this many are counted, not listed.
    static let failuresShown = 10
    public static let maxScreenshots = 3

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

    /// The facts, one per line, as the person sees them and as they reach
    /// the issue.
    public var details: String {
        var lines = [
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

    /// The body of `POST /v1/vault/feedback` (V34 API §6, V41): the person's
    /// words, these details, and the screenshots as base64, each typed by
    /// its first bytes.
    public func payload(description: String, screenshots: [Data]) -> [String: Any?] {
        [
            "description": description,
            "details": details,
            "app_version": appVersion,
            "build": build,
            "system": system,
            "screenshots": screenshots.prefix(Self.maxScreenshots).map { image in
                ["content_type": Self.imageType(of: image) ?? "application/octet-stream", "data": image.base64EncodedString()]
            },
        ]
    }

    /// `image/png` or `image/jpeg` from the signature, nil for anything else.
    public static func imageType(of data: Data) -> String? {
        if data.starts(with: [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) { return "image/png" }
        if data.starts(with: [0xFF, 0xD8, 0xFF]) { return "image/jpeg" }
        return nil
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
}
