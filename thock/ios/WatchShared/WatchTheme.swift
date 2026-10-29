import SwiftUI

/// The phone's dark tokens (V33 §13): the watch has no light mode, and
/// `Shared/Theme.swift` leans on UIKit's dynamic colours, which watchOS
/// lacks. Petrona for dates, SF for everything the person wrote.
enum WatchTheme {
    static func color(_ hex: UInt32) -> Color {
        Color(red: Double((hex >> 16) & 0xFF) / 255, green: Double((hex >> 8) & 0xFF) / 255, blue: Double(hex & 0xFF) / 255)
    }

    static let ground = color(0x151413)
    static let surface = color(0x1C1B19)
    static let rule = color(0x322E2A)
    static let ink = color(0xECE7DE)
    static let muted = color(0xA89F93)
    static let dim = color(0x6D675E)
    static let amber = color(0xE2A554)
    static let amberSoft = color(0x3A2E1C)
    static let amberInk = color(0x171613)
    static let cal = color(0x8FA5C0)
    static let good = color(0x86B48F)
    static let warn = color(0xE08A5A)

    /// Petrona, bundled with the watch app (`UIAppFonts`).
    static func serif(_ size: CGFloat, relativeTo style: Font.TextStyle = .title3) -> Font {
        .custom("Petrona", size: size, relativeTo: style).weight(.medium)
    }

    static func mono(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        .system(size: size, weight: weight, design: .monospaced)
    }
}

/// Where the watch app and its complications meet: one container, the
/// watch's two files, and the widgets' kinds.
enum WatchEnvironment {
    static let appGroup = "group.com.thethock.ios.watch"

    static let todayKind = "ThockToday"
    static let captureKind = "ThockCapture"

    static let captureURL = URL(string: "thock://capture?voice=1")
    static let todayURL = URL(string: "thock://day/today")

    static var container: URL {
        if let shared = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroup) {
            return shared
        }
        // Without the group (an unsigned build) the app keeps its files to
        // itself; only the complications lose sight of them.
        let fallback = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        do {
            try FileManager.default.createDirectory(at: fallback, withIntermediateDirectories: true)
        } catch {
            NSLog("Thock could not make its folder on the watch: \(error)")
        }
        return fallback
    }

    static var defaults: UserDefaults {
        UserDefaults(suiteName: appGroup) ?? .standard
    }

    /// Left by the Capture control for the app to pick up when it opens.
    static let pendingCaptureKey = "pending-capture"
}
