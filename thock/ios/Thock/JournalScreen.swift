import SwiftUI
import ThockKit

/// The journal (V33 §9): the day's entries in view, a new timestamped entry
/// started at the bottom of today's, and any earlier entry a tap away from
/// being fixed, with the bullets and quotes that belong to it (V46 §4.2).
struct JournalScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var day: VaultDay
    /// The entry the sheet opens on, or `nil` for a new one.
    var entryLine: Int?

    @State private var editing: JournalEntry?
    @State private var blocks: [Block] = []
    @State private var handle = EditorHandle()
    @State private var finished = false
    @State private var openedAt = Date()
    @State private var cursor: Int?
    @State private var owner = UUID()

    /// Only today takes a new entry: a stamp on another day would claim a
    /// time that never happened (V46 §4.3).
    private var isToday: Bool { day == .today() }

    private var clock: String {
        model.session?.writes(now: openedAt).clock ?? ""
    }

    private var leading: String {
        isToday ? "Today" : day.formatted("MMM D")
    }

    var body: some View {
        let _ = model.revision
        let journal = model.session?.view(day)?.journal
        VStack(spacing: 0) {
            SheetHeader(leading: leading, title: "Journal", trailing: "Done") {
                finish()
            } onTrailing: {
                finish()
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
                                    liveEntry(time: entry.time, initial: Self.opened(entry))
                                        .id("live")
                                } else {
                                    JournalEntryView(entry: entry, color: Theme.muted)
                                        .contentShape(Rectangle())
                                        .onTapGesture { switchTo(entry) }
                                        .accessibilityAddTraits(.isButton)
                                        .accessibilityHint("Edits this entry")
                                }
                            } else if journal?.entries.contains(where: { $0.contains(line: block.line) }) != true {
                                BlockView(block: block)
                            }
                        }
                        if editing == nil {
                            if isToday {
                                liveEntry(time: clock, initial: [])
                                    .id("live")
                            }
                        } else if isToday {
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
            cursor = model.takeRestoredCursor()
            if let entryLine {
                editing = journal?.written.first { $0.line == entryLine }
            }
            model.setLeaving(owner) { leave() }
        }
        .onDisappear {
            model.clearLeaving(owner)
            if !finished { commit() }
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
            RichTextEditor(initial: initial, handle: handle, placeholder: "A few lines about now…", fontSize: 17, scrolls: false, cursor: cursor) { query in
                model.session?.noteTitles(matching: query) ?? []
            } onChange: { blocks = $0 }
            .id(editing?.line ?? -1)
        }
    }

    /// The entry as the editor opens it: its paragraph without the stamp,
    /// then its bullets and quotes, soft-wrapped lines joined because the
    /// editor has no line breaks inside a block.
    static func opened(_ entry: JournalEntry) -> [Block] {
        [Block(id: 0, kind: .paragraph, text: entry.text.replacingOccurrences(of: "\n", with: " "))]
            + entry.attached.enumerated().map { offset, block in
                Block(id: offset + 1, kind: block.kind, text: block.text.replacingOccurrences(of: "\n", with: " "), indent: block.indent)
            }
    }

    private func switchTo(_ entry: JournalEntry?) {
        commit()
        blocks = []
        cursor = nil
        editing = entry
    }

    private func finish() {
        commit()
        finished = true
        dismiss()
    }

    /// Commits and stays (V46 §5.3): a new entry just written becomes the
    /// one being edited, so typing on after the return changes it instead
    /// of starting another.
    private func leave() -> LiveEditor {
        let position = handle.cursor
        let live = handle.blocks
        if !live.isEmpty {
            blocks = live
        }
        if commit() == .appended, let written = model.session?.journalEntryToContinue() {
            editing = written
        } else if let line = editing?.line {
            editing = model.session?.view(day)?.journal.written.first { $0.line == line }
        }
        return LiveEditor(line: editing?.line, cursor: position)
    }

    private enum Committed {
        case nothing
        case replaced
        case appended
    }

    /// Writes what the live editor holds: a replacement for the entry being
    /// edited, or a new entry.
    @discardableResult
    private func commit() -> Committed {
        let content = blocks.filter { $0.kind != .blank }
        guard !content.isEmpty else { return .nothing }
        let captured = blocks
        blocks = []
        if let entry = editing {
            let lines = EditorDocument(blocks: content).lines()
            let original = Self.opened(entry).map { block -> Block in
                var block = block
                block.touched = true
                return block
            }
            guard lines != EditorDocument(blocks: original).lines() else { return .nothing }
            let saved = model.perform { try $0.journalReplace(entry: entry, newText: lines.joined(separator: "\n"), day: day) }
            return saved ? .replaced : .nothing
        }
        guard isToday else { return .nothing }
        var saved = false
        model.perform { saved = try $0.journalAppend(blocks: captured, now: openedAt) }
        guard saved else { return .nothing }
        UINotificationFeedbackGenerator().notificationOccurred(.success)
        model.show("Added to today's journal")
        return .appended
    }
}
