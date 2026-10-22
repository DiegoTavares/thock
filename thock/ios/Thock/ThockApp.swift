import AppIntents
import SwiftUI
import ThockKit

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

/// The app icon's quick actions arrive through the scene delegate and the
/// silent push through this one; SwiftUI has no hook of its own for either.
final class AppDelegate: NSObject, UIApplicationDelegate {
    /// Owned here rather than by the scene: a silent push can launch the app
    /// in the background with no scene at all.
    @MainActor static let model = AppModel()
    @MainActor static var launchShortcut: String?

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        // A silent push needs no permission from the person, only the token.
        application.registerForRemoteNotifications()
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        let token = deviceToken.map { String(format: "%02x", $0) }.joined()
        Task { @MainActor in AppDelegate.model.received(pushToken: token) }
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        // A build signed without push lands here. Without a
        // token the phone hears about desk changes when it is next opened, as
        // it always has (V45 §10.6), so there is nothing to tell the person.
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
        Task { @MainActor in
            completionHandler(await AppDelegate.model.syncForPush() ? .newData : .noData)
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
