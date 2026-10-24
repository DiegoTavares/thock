import SwiftUI
import ThockKit
import UIKit

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
                case .journal(let day, let entry):
                    JournalScreen(day: day, entryLine: entry)
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
                case .section(let note, let key, let editing):
                    SectionScreen(note: note, key: key, editingLine: editing)
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

/// A window above every other, sheets included, that hides the vault while
/// the app is not frontmost and while a sheet that reads it waits behind
/// the lock (V46 §5.4). SwiftUI draws a sheet over anything the root view
/// overlays, so the cover needs a window of its own.
@MainActor
enum PrivacyCover {
    private static var window: UIWindow?

    static func update(covered: Bool, model: AppModel) {
        guard covered else {
            window?.isHidden = true
            window = nil
            return
        }
        guard window == nil,
              let scene = UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene }).first
        else { return }
        let cover = UIWindow(windowScene: scene)
        cover.windowLevel = .alert + 1
        let host = UIHostingController(rootView: CoverView().environment(model))
        host.view.backgroundColor = .clear
        cover.rootViewController = host
        cover.isHidden = false
        window = cover
    }
}

private struct CoverView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        ZStack {
            Theme.ground.ignoresSafeArea()
            VStack(spacing: 14) {
                Text("Thock")
                    .font(Theme.serif(40, style: .largeTitle))
                    .foregroundStyle(Theme.ink)
                if !model.isInactive {
                    Text("Your notes are waiting behind Face ID.")
                        .font(.system(size: 16))
                        .foregroundStyle(Theme.muted)
                        .multilineTextAlignment(.center)
                    Button("Show my notes") {
                        Task { await model.unlock() }
                    }
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(Theme.amber)
                    .padding(.top, 6)
                }
            }
            .padding(.horizontal, 32)
        }
        .preferredColorScheme(model.appearance.scheme)
        .tint(Theme.amber)
    }
}
