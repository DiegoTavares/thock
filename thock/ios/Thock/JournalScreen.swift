import SwiftUI
import ThockKit

/// The journal (V33 §9): the day's entries in view, a new timestamped
/// paragraph started at the bottom, and any earlier paragraph a tap away from
/// being fixed.
struct JournalScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase

    @State private var editing: JournalEntry?
    @State private var blocks: [Block] = []
    @State private var handle = EditorHandle()
    @State private var finished = false
    @State private var openedAt = Date()

    private var clock: String {
        model.session?.writes(now: openedAt).clock ?? ""
    }

    var body: some View {
        let _ = model.revision
        let journal = model.session?.view(.today())?.journal
        VStack(spacing: 0) {
            SheetHeader(leading: "Today", title: "Journal", trailing: "Done") {
                commit()
                finished = true
                dismiss()
            } onTrailing: {
                commit()
                finished = true
                dismiss()
            }
            .padding(.horizontal, 20)
            .padding(.bottom, 12)

            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 14) {
                        ForEach(journal?.blocks.filter { $0.kind != .blank && $0.kind != .rule } ?? []) { block in
                            if block.kind == .paragraph, let entry = journal?.entries.first(where: { $0.line == block.line }) {
                                if entry.isPrompt {
                                    EmptyView()
                                } else if entry.line == editing?.line {
                                    liveEntry(time: entry.time, initial: [Block(id: 0, kind: .paragraph, text: entry.text.replacingOccurrences(of: "\n", with: " "))])
                                        .id("live")
                                } else {
                                    JournalParagraph(entry: entry, color: Theme.muted)
                                        .contentShape(Rectangle())
                                        .onTapGesture { switchTo(entry) }
                                        .accessibilityHint("Edits this entry")
                                }
                            } else {
                                BlockView(block: block)
                            }
                        }
                        if editing == nil {
                            liveEntry(time: clock, initial: [])
                                .id("live")
                        } else {
                            Button("New entry") { switchTo(nil) }
                                .font(.system(size: 15, weight: .semibold))
                                .foregroundStyle(Theme.amber)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 20)
                    .padding(.bottom, 24)
                }
                .onAppear {
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) {
                        withAnimation { proxy.scrollTo("live", anchor: .bottom) }
                    }
                }
            }
        }
        .onAppear {
            openedAt = Date()
            // A thought typed in two bursts stays one thought.
            editing = model.session?.journalEntryToContinue()
        }
        .onDisappear {
            if !finished { commit() }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .background {
                commit()
                finished = true
                dismiss()
            }
        }
    }

    private func liveEntry(time: String?, initial: [Block]) -> some View {
        HStack(alignment: .top, spacing: 8) {
            if let time, !time.isEmpty {
                // Sits on the editor's first baseline: its inset plus the
                // difference between the two fonts' ascents.
                Text(time)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Theme.dim)
                    .padding(.top, 8)
            }
            RichTextEditor(initial: initial, handle: handle, placeholder: "A few lines about now…", fontSize: 17, scrolls: false) { query in
                model.session?.noteTitles(matching: query) ?? []
            } onChange: { blocks = $0 }
            .id(editing?.line ?? -1)
        }
    }

    private func switchTo(_ entry: JournalEntry?) {
        commit()
        blocks = []
        editing = entry
    }

    /// Writes what the live editor holds: a replacement for the paragraph
    /// being edited, or a new entry.
    private func commit() {
        let content = blocks.filter { $0.kind != .blank }
        guard !content.isEmpty else { return }
        let captured = blocks
        blocks = []
        if let entry = editing {
            let text = EditorDocument(blocks: content).lines().joined(separator: "\n")
            guard text != entry.text.replacingOccurrences(of: "\n", with: " ") else { return }
            model.perform { try $0.journalReplace(entry: entry, newText: text, day: .today()) }
        } else {
            var saved = false
            model.perform { saved = try $0.journalAppend(blocks: captured, now: openedAt) }
            if saved {
                UINotificationFeedbackGenerator().notificationOccurred(.success)
                model.show("Added to today's journal")
            }
        }
    }
}
