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
                    case .active:
                        model.isInactive = false
                        model.becameActive()
                    case .inactive:
                        model.isInactive = true
                    case .background:
                        model.isInactive = true
                        model.wentToBackground()
                    @unknown default:
                        break
                    }
                }
        }
    }
}

/// The app icon's quick actions arrive through the scene delegate, a
/// reminder's answers through the notification center's and the silent push
/// through this one; SwiftUI has no hook of its own for any of them.
final class AppDelegate: NSObject, UIApplicationDelegate {
    /// Owned here rather than by the scene: a notification's answer or a
    /// silent push can launch the app in the background with no scene at all.
    @MainActor static let model = AppModel()
    @MainActor static var launchShortcut: String?
    @MainActor static let notifier = ReminderNotifier()

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        // Set before launch ends, so an answer that launched the app arrives.
        UNUserNotificationCenter.current().delegate = AppDelegate.notifier
        AppDelegate.notifier.registerCategories(ReminderSettings())
        // So is a watch request that launched it (V48 §6.2).
        PhoneWatchLink.shared.activate()
        // A silent push needs no permission from the person, only the token.
        application.registerForRemoteNotifications()
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        let token = deviceToken.map { String(format: "%02x", $0) }.joined()
        Task { @MainActor in AppDelegate.model.received(pushToken: token) }
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        // A build signed without push, or a simulator, lands here. Without a
        // token the phone hears about desk changes when it is next opened.
        NSLog("Thock has no push token: \(error)")
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
            let answer = PushAnswer(completionHandler)
            // iOS gives a silent push thirty seconds and may stop waking an
            // app that runs over; a round on a slow network can take longer.
            let deadline = Task { @MainActor in
                try? await Task.sleep(nanoseconds: 25_000_000_000)
                guard !Task.isCancelled else { return }
                answer.finish(.failed)
            }
            answer.finish(await AppDelegate.model.syncInBackground() ? .newData : .noData)
            deadline.cancel()
        }
    }
}

/// A push's completion handler, called once: by the round or by the deadline.
@MainActor
private final class PushAnswer {
    private var completion: ((UIBackgroundFetchResult) -> Void)?

    init(_ completion: @escaping (UIBackgroundFetchResult) -> Void) {
        self.completion = completion
    }

    func finish(_ result: UIBackgroundFetchResult) {
        completion?(result)
        completion = nil
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
