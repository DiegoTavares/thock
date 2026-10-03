import SwiftUI
import ThockKit

/// The capture sheet (V33 §6): rises over whatever is open, keyboard up,
/// cursor blinking. Done saves; so does swiping it away.
struct CaptureSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    var entry: String
    var preset: CaptureDestination?

    @State private var destination: CaptureDestination = .inbox
    @State private var blocks: [Block] = []
    @State private var handle = EditorHandle()
    @State private var finished = false

    private var isEmpty: Bool {
        blocks.allSatisfy { $0.kind == .blank }
    }

    private var hint: String {
        switch destination {
        case .inbox: return "Lands in your inbox. Triage sorts it at the desk."
        case .today: return "One line joins today's plan. More than one lands in today's note as written."
        case .backlog: return "Goes under Soon in your backlog."
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            SheetHeader(leading: "Cancel", title: "New capture", trailing: "Done", trailingEnabled: !isEmpty) {
                finished = true
                dismiss()
            } onTrailing: {
                save()
                dismiss()
            }
            RichTextEditor(handle: handle, placeholder: "What's on your mind?", fontSize: 19) { query in
                model.session?.noteTitles(matching: query) ?? []
            } onChange: { blocks = $0 }
            .frame(maxHeight: .infinity)

            HStack(spacing: 8) {
                ForEach(CaptureDestination.allCases, id: \.self) { option in
                    Button {
                        destination = option
                        UISelectionFeedbackGenerator().selectionChanged()
                    } label: {
                        Text(option.title)
                            .font(.system(size: 14, weight: .medium))
                            .foregroundStyle(destination == option ? Theme.amberInk : Theme.muted)
                            .padding(.horizontal, 14)
                            .padding(.vertical, 7)
                            .background(destination == option ? Theme.amber : .clear, in: Capsule())
                            .overlay(Capsule().stroke(destination == option ? Theme.amber : Theme.rule, lineWidth: 1))
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(destination == option ? .isSelected : [])
                }
            }
            Text(hint)
                .font(.system(size: 13))
                .foregroundStyle(Theme.dim)
                .padding(.bottom, 10)
        }
        .padding(.horizontal, 20)
        .onAppear {
            destination = preset ?? model.chip(for: entry)
        }
        .onDisappear(perform: save)
        .onChange(of: scenePhase) { _, phase in
            // Locking the phone mid-thought keeps the thought.
            if phase == .background, !isEmpty {
                save()
                dismiss()
            }
        }
    }

    private func save() {
        guard !finished else { return }
        finished = true
        guard !isEmpty else { return }
        model.saveCapture(blocks: blocks, destination: destination, entry: entry)
    }
}

extension CaptureDestination {
    var title: String {
        switch self {
        case .inbox: return "Inbox"
        case .today: return "Today"
        case .backlog: return "Backlog"
        }
    }
}
