import AppIntents
import SwiftUI
import ThockKit
import UserNotifications

@main
struct ThockApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @Environment(\.scenePhase) private var scenePhase

    init() {
        Theme.registerFonts()
    }

    private var model: AppModel { AppDelegate.model }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .preferredColorScheme(model.appearance.scheme)
                .tint(Theme.amber)
                .task {
                    await model.boot()
                    if let shortcut = AppDelegate.launchShortcut {
                        AppDelegate.launchShortcut = nil
                        model.handle(shortcut: shortcut)
                    }
                }
                .onOpenURL { model.handle(url: $0) }
                .onChange(of: scenePhase) { _, phase in
                    switch phase {
                    case .active: model.becameActive()
                    case .background: model.wentToBackground()
                    default: break
                    }
                }
        }
    }
}

/// The app icon's quick actions arrive through the scene delegate and a
/// reminder's answers through the notification center's; SwiftUI has no
/// hook of its own for either.
final class AppDelegate: NSObject, UIApplicationDelegate {
    /// Owned here rather than by the scene: a notification's answer can
    /// launch the app in the background with no scene at all.
    @MainActor static let model = AppModel()
    @MainActor static var launchShortcut: String?
    @MainActor static let notifier = ReminderNotifier()

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        // Set before launch ends, so an answer that launched the app arrives.
        UNUserNotificationCenter.current().delegate = AppDelegate.notifier
        AppDelegate.notifier.registerCategories(ReminderSettings())
        return true
    }

    func application(_ application: UIApplication, configurationForConnecting session: UISceneSession, options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        if let shortcut = options.shortcutItem {
            Task { @MainActor in AppDelegate.launchShortcut = shortcut.type }
        }
        let configuration = UISceneConfiguration(name: nil, sessionRole: session.role)
        configuration.delegateClass = SceneDelegate.self
        return configuration
    }

    func application(_ application: UIApplication, didReceiveRemoteNotification userInfo: [AnyHashable: Any], fetchCompletionHandler completionHandler: @escaping (UIBackgroundFetchResult) -> Void) {
        // The server's silent push is a nudge to pull (V34 API §6.5).
        Task { @MainActor in
            AppDelegate.model.becameActive()
            completionHandler(.newData)
        }
    }
}

final class SceneDelegate: NSObject, UIWindowSceneDelegate {
    func windowScene(_ windowScene: UIWindowScene, performActionFor shortcutItem: UIApplicationShortcutItem, completionHandler: @escaping (Bool) -> Void) {
        let type = shortcutItem.type
        Task { @MainActor in
            AppDelegate.model.handle(shortcut: type)
            completionHandler(true)
        }
    }
}

/// What makes the capture sheet assignable to the Action Button and
/// reachable from Shortcuts and Spotlight.
struct ThockShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(intent: NewIdeaIntent(), phrases: ["New idea in \(.applicationName)", "Capture in \(.applicationName)"], shortTitle: "New Idea", systemImageName: "plus")
        AppShortcut(intent: JournalIntent(), phrases: ["Journal in \(.applicationName)"], shortTitle: "Journal", systemImageName: "text.alignleft")
    }
}
