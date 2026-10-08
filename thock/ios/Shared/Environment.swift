import Foundation
import ThockKit

/// Where the app, the share sheet and the widgets meet: one container, one
/// store, one place to leave a note for the app about what to open.
enum ThockEnvironment {
    static let appGroup = "group.com.thethock.ios"
    /// Where *Report a problem* sends its email: the address on the support page.
    static let supportEmail = "diego.exodo@gmail.com"

    static var container: URL {
        if let shared = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroup) {
            return shared
        }
        // Without the group (a build signed without it) the app still works
        // on its own; only the extensions lose sight of the vault.
        let fallback = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        try? FileManager.default.createDirectory(at: fallback, withIntermediateDirectories: true)
        return fallback
    }

    static var defaults: UserDefaults {
        UserDefaults(suiteName: appGroup) ?? .standard
    }

    static func openStore() throws -> VaultStore {
        try VaultStore(url: container.appendingPathComponent("vault.sqlite"))
    }

    static var practiceWorldURL: URL {
        container.appendingPathComponent("practice-desk.json")
    }
}

/// What an entry point outside the app asks it to open (V33 §12).
enum EntryPoint: String, Sendable {
    case idea
    case journal
    case clip
    case ask

    private static let key = "pending-entry"

    /// Left by a control or shortcut for the app to pick up when it opens.
    static func leave(_ entry: EntryPoint) {
        ThockEnvironment.defaults.set(entry.rawValue, forKey: key)
    }

    static func take() -> EntryPoint? {
        let defaults = ThockEnvironment.defaults
        guard let raw = defaults.string(forKey: key) else { return nil }
        defaults.removeObject(forKey: key)
        return EntryPoint(rawValue: raw)
    }
}
