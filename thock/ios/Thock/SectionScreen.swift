import SwiftUI
import ThockKit

/// A run of the person's prose open for one block (V37 §6, §8; V46 §4.5): a
/// section of a day or a week, the text above its first section, or the
/// notes under a planner group. The blocks are in view, dimmed, with the
/// tapped one live, or a new one started at the bottom. Done writes only
/// the block that changed; the heading and everything around it stay as
/// they are.
struct SectionScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var note: NoteID
    var key: SectionKey
    var editingLine: Int?

    @State private var editing: Int?
    @State private var blocks: [Block] = []
    @State private var handle = EditorHandle()
    @State private var finished = false
    @State private var cursor: Int?
    /// Bumped to open the live editor afresh on the same line.
    @State private var generation = 0
    @State private var owner = UUID()

    private var leading: String {
        switch note {
        case .day(let day): return day == .today() ? "Today" : day.formatted("MMM D")
        case .week(let week): return "Week \(week.week)"
        }
    }

    var body: some View {
        let _ = model.revision
        let section = model.session?.view(note)?.section(key)
        VStack(spacing: 0) {
            SheetHeader(leading: leading, title: section?.title ?? "", trailing: "Done") {
                finish()
            } onTrailing: {
                finish()
            }
            .padding(.horizontal, 20)
            .padding(.bottom, 12)

            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 14) {
                        if let section {
                            ForEach(section.blocks.filter { $0.kind != .blank && $0.kind != .rule }) { block in
                                if block.line == editing {
                                    liveParagraph(initial: [Self.opened(block)])
                                        .id("live")
                                } else if section.isEditable(block) {
                                    BlockView(block: block)
                                        .foregroundStyle(Theme.muted)
                                        .contentShape(Rectangle())
                                        .onTapGesture { switchTo(block.line) }
                                        .accessibilityAddTraits(.isButton)
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
        .onAppear {
            editing = editingLine
            cursor = model.takeRestoredCursor()
            model.setLeaving(owner) { leave() }
        }
        .onDisappear {
            model.clearLeaving(owner)
            if !finished { commit() }
        }
    }

    /// The block as the editor opens it: the same kind and indent, so a
    /// bullet or a quote is written back as one, with soft-wrapped lines
    /// joined because the editor has no line breaks inside a paragraph.
    private static func opened(_ block: Block) -> Block {
        Block(id: 0, kind: block.kind, text: block.text.replacingOccurrences(of: "\n", with: " "), indent: block.indent)
    }

    private func liveParagraph(initial: [Block]) -> some View {
        RichTextEditor(initial: initial, handle: handle, placeholder: "A few lines…", fontSize: 17, scrolls: false, cursor: cursor) { query in
            model.session?.noteTitles(matching: query) ?? []
        } onChange: { blocks = $0 }
        .id([editing ?? -1, generation])
    }

    private func switchTo(_ line: Int?) {
        commit()
        blocks = []
        cursor = nil
        editing = line
    }

    private func finish() {
        commit()
        finished = true
        dismiss()
    }

    /// Commits and stays (V46 §5.3): a paragraph just added becomes the one
    /// being edited, so typing on after the return changes it.
    private func leave() -> LiveEditor {
        let position = handle.cursor
        let live = handle.blocks
        if !live.isEmpty {
            blocks = live
        }
        let committed = commit()
        if committed == .appended {
            editing = model.session?.lastEditableLine(in: key, note: note)
        }
        if committed != .nothing {
            let target = editing.flatMap { line in model.session?.view(note)?.section(key)?.blocks.first { $0.line == line } }
            settle(live: live, opens: target.map { [Self.opened($0)] } ?? [], cursor: position)
        }
        return LiveEditor(line: editing, cursor: position)
    }

    /// The editor stays open on `editing` after a commit on the way out, so
    /// it must hold exactly that block: an edit that became two blocks would
    /// otherwise write the second again on the next commit. When it holds
    /// more, it opens afresh on the block alone.
    private func settle(live: [Block], opens target: [Block], cursor position: Int?) {
        if EditorDocument(blocks: live.filter { $0.kind != .blank }).lines() == EditorDocument(blocks: target).lines() {
            cursor = position
        } else {
            cursor = nil
            generation += 1
        }
    }

    private enum Committed {
        case nothing
        case replaced
        case appended
    }

    /// Writes what the live editor holds: a replacement for the block being
    /// edited, or a new paragraph where the section takes one.
    @discardableResult
    private func commit() -> Committed {
        let content = blocks.filter { $0.kind != .blank }
        guard !content.isEmpty, let section = model.session?.view(note)?.section(key) else { return .nothing }
        let captured = blocks
        blocks = []
        if let line = editing, let block = section.blocks.first(where: { $0.line == line }) {
            let lines = EditorDocument(blocks: content).lines()
            var original = Self.opened(block)
            original.touched = true
            guard lines != original.markdownLines() else { return .nothing }
            let saved = model.perform { try $0.replaceParagraph(block, in: section, with: lines.joined(separator: "\n"), note: note) }
            return saved ? .replaced : .nothing
        }
        var saved = false
        model.perform { saved = try $0.appendParagraph(blocks: captured, to: section, note: note) }
        guard saved else { return .nothing }
        UINotificationFeedbackGenerator().notificationOccurred(.success)
        model.show(section.title.isEmpty ? "Added to the note" : "Added to \(section.title)")
        return .appended
    }
}
