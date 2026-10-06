import SwiftUI
import ThockKit

/// A prose section of a day or a week, open for one paragraph (V37 §6, §8):
/// the section's paragraphs in view, dimmed, with the tapped one live, or a
/// new one started at the bottom. Done writes only the paragraph that
/// changed; the heading and everything around it stay as they are.
struct SectionScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase

    var note: NoteID
    var cardID: Int
    var editingLine: Int?

    @State private var editing: Int?
    @State private var blocks: [Block] = []
    @State private var handle = EditorHandle()
    @State private var finished = false

    private var leading: String {
        switch note {
        case .day(let day): return day == .today() ? "Today" : day.formatted("MMM D")
        case .week(let week): return "Week \(week.week)"
        }
    }

    var body: some View {
        let _ = model.revision
        let view = model.session?.view(note)
        let card = view?.cards.first { $0.id == cardID }
        VStack(spacing: 0) {
            SheetHeader(leading: leading, title: card?.title ?? "", trailing: "Done") {
                finish()
            } onTrailing: {
                finish()
            }
            .padding(.horizontal, 20)
            .padding(.bottom, 12)

            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 14) {
                        if let view, let card {
                            ForEach(card.blocks.filter { $0.kind != .blank && $0.kind != .rule }) { block in
                                if block.line == editing {
                                    liveParagraph(initial: [Self.opened(block)])
                                        .id("live")
                                } else if view.isEditable(block, in: card) {
                                    BlockView(block: block)
                                        .foregroundStyle(Theme.muted)
                                        .contentShape(Rectangle())
                                        .onTapGesture { switchTo(block.line) }
                                        .accessibilityHint("Edits this line")
                                } else {
                                    BlockView(block: block)
                                }
                            }
                        }
                        if editing == nil {
                            liveParagraph(initial: [])
                                .id("live")
                        } else {
                            Button("New paragraph") { switchTo(nil) }
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
        .onAppear { editing = editingLine }
        .onDisappear {
            if !finished { commit() }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .background { finish() }
        }
    }

    /// The block as the editor opens it: the same kind and indent, so a
    /// bullet or a quote is written back as one, with soft-wrapped lines
    /// joined because the editor has no line breaks inside a paragraph.
    private static func opened(_ block: Block) -> Block {
        Block(id: 0, kind: block.kind, text: block.text.replacingOccurrences(of: "\n", with: " "), indent: block.indent)
    }

    private func liveParagraph(initial: [Block]) -> some View {
        RichTextEditor(initial: initial, handle: handle, placeholder: "A few lines…", fontSize: 17, scrolls: false) { query in
            model.session?.noteTitles(matching: query) ?? []
        } onChange: { blocks = $0 }
        .id(editing ?? -1)
    }

    private func switchTo(_ line: Int?) {
        commit()
        blocks = []
        editing = line
    }

    private func finish() {
        commit()
        finished = true
        dismiss()
    }

    /// Writes what the live editor holds: a replacement for the paragraph
    /// being edited, or a new paragraph at the end of the section.
    private func commit() {
        let content = blocks.filter { $0.kind != .blank }
        guard !content.isEmpty, let view = model.session?.view(note), let card = view.cards.first(where: { $0.id == cardID }) else { return }
        let captured = blocks
        blocks = []
        if let line = editing, let block = card.blocks.first(where: { $0.line == line }) {
            let lines = EditorDocument(blocks: content).lines()
            var original = Self.opened(block)
            original.touched = true
            guard lines != original.markdownLines() else { return }
            model.perform { try $0.replaceParagraph(block, in: card, with: lines.joined(separator: "\n"), note: note) }
        } else {
            var saved = false
            model.perform { saved = try $0.appendParagraph(blocks: captured, to: card, note: note) }
            if saved {
                UINotificationFeedbackGenerator().notificationOccurred(.success)
                model.show("Added to \(card.title)")
            }
        }
    }
}
