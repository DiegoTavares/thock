import AppIntents
import Foundation

/// Opens Thock on the watch already listening: behind the Capture control
/// in Control Center, the Smart Stack and the Action button (V48 §7.4).
struct CaptureIntent: AppIntent {
    static let title: LocalizedStringResource = "Capture"
    static let description = IntentDescription("Opens Thock already listening.")
    static let openAppWhenRun = true

    @MainActor
    func perform() async throws -> some IntentResult {
        // Taken by the app as it comes forward.
        WatchEnvironment.defaults.set(true, forKey: WatchEnvironment.pendingCaptureKey)
        NotificationCenter.default.post(name: .thockWatchCapture, object: nil)
        return .result()
    }
}

extension Notification.Name {
    static let thockWatchCapture = Notification.Name("ThockWatchCapture")
}
