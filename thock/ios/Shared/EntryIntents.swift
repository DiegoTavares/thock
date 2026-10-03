import AppIntents
import Foundation

/// Opens Thock straight into the capture sheet. Behind the Lock Screen
/// control, the Action Button and the app shortcut.
struct NewIdeaIntent: AppIntent {
    static let title: LocalizedStringResource = "New Idea"
    static let description = IntentDescription("Opens a blank capture, keyboard up.")
    static let openAppWhenRun = true

    @MainActor
    func perform() async throws -> some IntentResult {
        EntryPoint.leave(.idea)
        NotificationCenter.default.post(name: .thockEntryPoint, object: nil)
        return .result()
    }
}

/// Opens today's journal with a new entry started.
struct JournalIntent: AppIntent {
    static let title: LocalizedStringResource = "Journal"
    static let description = IntentDescription("Opens today's journal with a new entry started.")
    static let openAppWhenRun = true

    @MainActor
    func perform() async throws -> some IntentResult {
        EntryPoint.leave(.journal)
        NotificationCenter.default.post(name: .thockEntryPoint, object: nil)
        return .result()
    }
}

extension Notification.Name {
    static let thockEntryPoint = Notification.Name("ThockEntryPoint")
}
