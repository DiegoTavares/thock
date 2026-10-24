import SwiftUI
import ThockKit

/// The capture sheet (V33 §6): rises over whatever is open, keyboard up,
/// cursor blinking. Done saves; so does swiping it away. Leaving the app
/// keeps it open with what it holds, written nowhere until saved (V46 §5.3).
struct CaptureSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    var entry: String
    var preset: CaptureDestination?

    @State private var destination: CaptureDestination = .today
    /// The chip to fall back to when *Remind me* closes without a time.
    @State private var beforeReminder: CaptureDestination = .today
    @State private var due: ReminderDue?
    @State private var choosingTime = false
    @State private var blocks: [Block] = []
    @State private var images: [ImageAttachment] = []
    @State private var handle = EditorHandle()
    @State private var finished = false
    @State private var initial: [Block] = []
    @State private var owner = UUID()

    private var isEmpty: Bool {
        blocks.allSatisfy { $0.kind == .blank } && images.isEmpty
    }

    private var hint: String {
        switch destination {
        case .inbox: return "Lands in your inbox. Triage sorts it at the desk."
        case .today: return "One line joins today's plan. More than one lands in today's note as written."
        case .backlog: return "Goes under Soon in your backlog."
        case .reminder:
            let settings = model.session?.config.reminders ?? ReminderSettings()
            guard let due else { return "Rings when you choose and waits in \(settings.index)." }
            guard due.stillRings(today: .today(), now: .now(), dateOnlyAt: settings.dateOnlyAt) else {
                return "Waits in Today, in \(settings.index)."
            }
            return "\(due.ringsSentence(today: .today(), now: .now(), dateOnlyAt: settings.dateOnlyAt)) and waits in \(settings.index)."
        }
    }

    private func choose(_ option: CaptureDestination) {
        UISelectionFeedbackGenerator().selectionChanged()
        guard option == .reminder else {
            destination = option
            return
        }
        if destination != .reminder {
            beforeReminder = destination
        }
        destination = .reminder
        choosingTime = true
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
            RichTextEditor(initial: initial, handle: handle, placeholder: "What's on your mind?", fontSize: 19) { query in
                model.session?.noteTitles(matching: query) ?? []
            } onChange: { blocks = $0 }
            .id(initial.isEmpty ? 0 : 1)
            .frame(maxHeight: .infinity)

            AttachmentStrip(images: $images)

            HStack(spacing: 8) {
                ForEach(CaptureDestination.allCases, id: \.self) { option in
                    Button {
                        choose(option)
                    } label: {
                        Text(option == .reminder ? (due?.chipText(today: .today()) ?? option.title) : option.title)
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
                // Every hint gets two lines so switching chips never shifts them.
                .lineLimit(2, reservesSpace: true)
                .padding(.bottom, 10)
        }
        .padding(.horizontal, 20)
        .sheet(isPresented: $choosingTime, onDismiss: {
            // Closed without a time: the capture goes where it was going.
            if due == nil {
                destination = beforeReminder
            }
        }) {
            RemindMeSheet(subject: "", initial: due) { due = $0 }
                .presentationDetents([.height(360)])
                .presentationBackground(Theme.surface)
                .presentationCornerRadius(26)
                .preferredColorScheme(model.appearance.scheme)
        }
        .onAppear {
            destination = preset ?? model.chip(for: entry)
            if let draft = model.takeRestoredDraft() {
                restore(draft)
            }
            model.setLeaving(owner) { LiveEditor(cursor: handle.cursor, draft: draft()) }
        }
        .onDisappear {
            model.clearLeaving(owner)
            save()
        }
    }

    private func draft() -> ResumePoint.Draft {
        var draft = ResumePoint.Draft()
        let live = handle.blocks
        draft.markdown = EditorDocument(blocks: live.isEmpty ? blocks : live).lines().joined(separator: "\n")
        draft.destination = destination
        draft.due = due?.description
        draft.images = images
        return draft
    }

    private func restore(_ draft: ResumePoint.Draft) {
        initial = Blocks.parse(draft.markdown).filter { $0.kind != .blank }
        blocks = initial
        destination = draft.destination ?? destination
        due = draft.due.flatMap(ReminderDue.init)
        if destination == .reminder, due == nil {
            destination = beforeReminder
        }
        images = draft.images
    }

    private func save() {
        guard !finished else { return }
        finished = true
        guard !isEmpty else { return }
        // A reminder with no time yet is not a reminder: it goes where the
        // capture was going before the chip was touched.
        let target = destination == .reminder && due == nil ? beforeReminder : destination
        model.saveCapture(blocks: blocks, destination: target, images: images, due: due, entry: entry)
    }
}

extension CaptureDestination {
    var title: String {
        switch self {
        case .inbox: return "Inbox"
        case .today: return "Today"
        case .backlog: return "Backlog"
        case .reminder: return "Reminder"
        }
    }
}
