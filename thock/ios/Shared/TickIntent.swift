import AppIntents
import ThockKit
import WidgetKit

/// Ticks one line of today's plan from a widget. The change lands in the
/// shared store like any other and reaches the desk when the app next runs.
struct TickLineIntent: AppIntent {
    static let title: LocalizedStringResource = "Tick a Plan Line"
    static let isDiscoverable = false

    @Parameter(title: "Line")
    var hash: String

    @Parameter(title: "Which")
    var ordinal: Int

    @Parameter(title: "Day")
    var day: String

    init() {}

    init(hash: String, ordinal: Int, day: VaultDay) {
        self.hash = hash
        self.ordinal = ordinal
        self.day = day.iso
    }

    func perform() async throws -> some IntentResult {
        let store = try ThockEnvironment.openStore()
        if let day = VaultDay(iso: day) {
            try VaultSession(store: store).tick(hash: hash, ordinal: ordinal, day: day)
        }
        WidgetCenter.shared.reloadAllTimelines()
        return .result()
    }
}
