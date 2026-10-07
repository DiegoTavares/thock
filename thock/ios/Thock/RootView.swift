import SwiftUI
import ThockKit

struct RootView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        ZStack {
            Theme.ground.ignoresSafeArea()
            switch model.phase {
            case .loading:
                EmptyView()
            case .welcome:
                WelcomeView()
            case .ready:
                TodayScreen()
            }
        }
        .overlay(alignment: .bottom) {
            if let toast = model.toast {
                ToastView(toast: toast)
                    .padding(.bottom, 84)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .sheet(item: $model.sheet) { sheet in
            Group {
                switch sheet {
                case .capture(let entry, let preset):
                    CaptureSheet(entry: entry, preset: preset)
                case .journal:
                    JournalScreen()
                case .clip:
                    InAppClipSheet()
                case .ask:
                    AskScreen()
                case .receipts:
                    ReceiptsScreen()
                case .you:
                    YouSheet()
                case .calendar:
                    CalendarSheet()
                        .presentationDetents([.height(430)])
                case .section(let note, let card, let editing):
                    SectionScreen(note: note, cardID: card, editingLine: editing)
                }
            }
            .presentationBackground(Theme.surface)
            .presentationCornerRadius(26)
            .presentationDragIndicator(.hidden)
            .preferredColorScheme(model.appearance.scheme)
            .tint(Theme.amber)
        }
    }
}

struct ToastView: View {
    var toast: Toast

    var body: some View {
        HStack(spacing: 14) {
            Text(toast.text)
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(Theme.ink)
            if let undo = toast.undo {
                Button(toast.action, action: undo)
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(Theme.amber)
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
        .background(Theme.surface, in: Capsule())
        .overlay(Capsule().stroke(Theme.rule, lineWidth: 1))
        .shadow(color: .black.opacity(0.25), radius: 16, y: 6)
        .padding(.horizontal, 20)
        .accessibilityElement(children: .combine)
    }
}
