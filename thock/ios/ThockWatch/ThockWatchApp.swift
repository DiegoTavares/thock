import SwiftUI
import ThockKit
import WatchKit

/// Thock on the wrist (V48): today's plan, ticks, and a capture that
/// listens. A satellite of the phone, which does every read and write.
@main
struct ThockWatchApp: App {
    @WKApplicationDelegateAdaptor(WatchDelegate.self) private var delegate
    @Environment(\.scenePhase) private var scenePhase

    private var model: WatchModel { WatchModel.shared }

    var body: some Scene {
        WindowGroup {
            DayScreen()
                .environment(model)
                .tint(WatchTheme.amber)
                .onOpenURL { model.handle($0) }
                // The Capture control, run while the app is already up.
                .onReceive(NotificationCenter.default.publisher(for: .thockWatchCapture)) { _ in model.becameActive() }
                .onChange(of: scenePhase) { _, phase in
                    if phase == .active {
                        model.becameActive()
                    }
                }
        }
    }
}

/// Activates the link at launch, so a snapshot or an answer that woke the
/// app in the background is received, and finishes the system's
/// background refreshes once they are handled.
final class WatchDelegate: NSObject, WKApplicationDelegate {
    func applicationDidFinishLaunching() {
        Task { @MainActor in WatchModel.shared.start() }
    }

    func handle(_ backgroundTasks: Set<WKRefreshBackgroundTask>) {
        for task in backgroundTasks {
            if let refresh = task as? WKWatchConnectivityRefreshBackgroundTask {
                Task { @MainActor in WatchModel.shared.connection.hold(refresh) }
            } else {
                task.setTaskCompletedWithSnapshot(false)
            }
        }
    }
}
