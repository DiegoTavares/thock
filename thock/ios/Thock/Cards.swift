import SwiftUI
import ThockKit

struct CardView: View {
    @Environment(AppModel.self) private var model
    var card: NoteCard
    var view: NoteView
    var day: VaultDay

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Hairline()
            switch card.kind {
            case .journal:
                JournalCard(card: card, journal: view.journal, day: day)
            case .planner:
                PlannerCard(planner: view.planner, day: day, title: card.title)
            case .agent:
                CardLabel(title: card.title)
                VStack(alignment: .leading, spacing: 8) {
                    BlockList(blocks: card.blocks)
                }
                .padding(.vertical, 10)
                .padding(.horizontal, 12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Theme.amberSoft)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.amber).frame(width: 2) }
                .clipShape(UnevenRoundedRectangle(bottomTrailingRadius: 10, topTrailingRadius: 10))
            case .preamble:
                BlockList(blocks: card.blocks)
            case .prose:
                CardLabel(title: card.title)
                BlockList(blocks: card.blocks)
            }
        }
    }
}

/// Read-only rendering of a run of blocks: prose as prose, anything outside
/// the phone's vocabulary as a grey block.
struct BlockList: View {
    var blocks: [Block]

    var body: some View {
        let visible = blocks.filter { $0.kind != .blank && $0.kind != .rule }
        ForEach(visible) { block in
            BlockView(block: block)
        }
    }
}

struct BlockView: View {
    var block: Block

    var body: some View {
        switch block.kind {
        case .paragraph:
            let prompt = isPrompt
            Text(Rich.text(block.runs))
                .font(.system(size: prompt ? 15 : 17))
                .foregroundStyle(prompt ? Theme.dim : Theme.ink)
                .lineSpacing(3)
                .fixedSize(horizontal: false, vertical: true)
        case .heading:
            Text(Rich.text(block.runs))
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(Theme.muted)
                .padding(.top, 4)
        case .bullet:
            marked("•")
        case .numbered(let marker):
            marked(marker)
        case .task(let checked):
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Checkbox(checked: checked, size: 18)
                    .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 3 }
                Text(Rich.text(block.runs))
                    .font(.system(size: 17))
                    .foregroundStyle(checked ? Theme.dim : Theme.ink)
                    .strikethrough(checked, color: Theme.dim)
            }
            .padding(.leading, indent)
        case .quote:
            Text(Rich.text(block.runs))
                .font(.system(size: 17))
                .foregroundStyle(Theme.muted)
                .lineSpacing(3)
                .padding(.leading, 12)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.rule).frame(width: 2) }
        case .opaque:
            OpaqueBlock(lines: block.source)
        case .rule, .blank:
            EmptyView()
        }
    }

    private var indent: CGFloat {
        CGFloat(block.indent.reduce(0) { $0 + ($1 == "\t" ? 4 : 1) }) * 7
    }

    /// A template's italic hint (`_What happened…_`) reads as a hint.
    private var isPrompt: Bool {
        let runs = block.runs
        return !runs.isEmpty && runs.allSatisfy { $0.italic || $0.code || $0.text.allSatisfy(\.isWhitespace) }
    }

    private func marked(_ marker: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(marker)
                .font(.system(size: 17))
                .foregroundStyle(Theme.muted)
            Text(Rich.text(block.runs))
                .font(.system(size: 17))
                .foregroundStyle(Theme.ink)
                .lineSpacing(3)
        }
        .padding(.leading, indent)
    }
}

/// A table, code block or comment: visible, preserved, not editable here.
struct OpaqueBlock: View {
    var lines: [String]

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(lines.first?.trimmingCharacters(in: .whitespaces) ?? "")
                .font(Theme.mono(12))
                .foregroundStyle(Theme.muted)
                .lineLimit(1)
            if lines.count > 1 {
                Text("\(lines.count) lines, kept as written at the desk")
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.dim)
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.sunken, in: RoundedRectangle(cornerRadius: 8))
        .accessibilityLabel("A block kept as written at the desk")
    }
}

struct Checkbox: View {
    var checked: Bool
    var size: CGFloat = 20

    var body: some View {
        RoundedRectangle(cornerRadius: size * 0.3)
            .fill(checked ? Theme.amber : .clear)
            .overlay(RoundedRectangle(cornerRadius: size * 0.3).stroke(checked ? Theme.amber : Theme.muted, lineWidth: 1.5))
            .overlay {
                if checked {
                    Image(systemName: "checkmark")
                        .font(.system(size: size * 0.55, weight: .bold))
                        .foregroundStyle(Theme.amberInk)
                }
            }
            .frame(width: size, height: size)
    }
}

enum Rich {
    /// Inline runs as styled text: bold, italic, code and links the way the
    /// desk's conceal mode shows them.
    static func text(_ runs: [InlineRun]) -> AttributedString {
        var output = AttributedString()
        for run in runs {
            var piece = AttributedString(run.text)
            var intent: InlinePresentationIntent = []
            if run.bold { intent.insert(.stronglyEmphasized) }
            if run.italic { intent.insert(.emphasized) }
            if run.code { intent.insert(.code) }
            if run.strike { intent.insert(.strikethrough) }
            if !intent.isEmpty { piece.inlinePresentationIntent = intent }
            switch run.link {
            case .note?: piece.foregroundColor = Theme.amber
            case .web?: piece.foregroundColor = Theme.cal
            case nil: break
            }
            output += piece
        }
        return output
    }
}

// MARK: Journal

struct JournalCard: View {
    @Environment(AppModel.self) private var model
    var card: NoteCard
    var journal: Journal
    var day: VaultDay

    var body: some View {
        let editable = day == .today() && !model.isReadOnly
        VStack(alignment: .leading, spacing: 10) {
            CardLabel(title: card.title)
            let written = journal.written
            if written.isEmpty {
                Text(Rich.text(Inline.parse(journal.entries.first { $0.isPrompt }?.text.replacingOccurrences(of: "\n", with: " ") ?? "Nothing yet.")))
                    .font(.system(size: 15))
                    .foregroundStyle(Theme.dim)
            }
            ForEach(card.blocks.filter { $0.kind != .blank && $0.kind != .rule }) { block in
                if let entry = journal.entries.first(where: { $0.line == block.line }), block.kind == .paragraph {
                    if !entry.isPrompt {
                        JournalParagraph(entry: entry)
                    }
                } else {
                    BlockView(block: block)
                }
            }
        }
        .contentShape(Rectangle())
        .onTapGesture {
            if editable { model.sheet = .journal }
        }
        .accessibilityHint(editable ? "Opens the journal" : "")
    }
}

struct JournalParagraph: View {
    var entry: JournalEntry
    var color = Theme.ink

    var body: some View {
        let body = Rich.text(Inline.parse(entry.text.replacingOccurrences(of: "\n", with: " ")))
        Group {
            if let time = entry.time {
                Text(time).font(.system(size: 13, weight: .semibold)).foregroundStyle(Theme.dim) + Text("  ") + Text(body)
            } else {
                Text(body)
            }
        }
        .font(.system(size: 17))
        .foregroundStyle(color)
        .lineSpacing(3)
        .fixedSize(horizontal: false, vertical: true)
    }
}

// MARK: Plan

struct PlannerCard: View {
    @Environment(AppModel.self) private var model
    var planner: Planner
    var day: VaultDay
    var title: String

    @State private var timing: PlannerItem?
    @State private var editing: PlannerItem?

    var body: some View {
        let items = planner.items.filter { !model.isBeingRemoved($0, day: day) }
        VStack(alignment: .leading, spacing: 4) {
            CardLabel(title: title, note: items.isEmpty ? nil : "\(items.filter(\.done).count) of \(items.count)")
                .padding(.bottom, 4)
            ForEach(planner.groups) { group in
                if let name = group.name {
                    Text(name)
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(group.isCalendar ? Theme.cal : PlannerPalette.color(for: name))
                        .padding(.top, 8)
                        .padding(.bottom, 2)
                }
                ForEach(group.items.filter { !model.isBeingRemoved($0, day: day) }) { item in
                    PlannerRow(item: item, day: day, timing: $timing, editing: $editing)
                }
                if !group.isCalendar, !model.isReadOnly {
                    AddLineRow(group: group, day: day)
                }
            }
        }
        .sheet(item: $timing) { item in
            SetTimeSheet(item: item, day: day)
                .presentationDetents([.height(360)])
                .presentationBackground(Theme.surface)
                .presentationCornerRadius(26)
                .preferredColorScheme(model.appearance.scheme)
        }
        .sheet(item: $editing) { item in
            EditLineSheet(item: item, day: day)
                .presentationDetents([.height(220)])
                .presentationBackground(Theme.surface)
                .presentationCornerRadius(26)
                .preferredColorScheme(model.appearance.scheme)
        }
    }
}

/// The desk hashes a subsection's name into its palette (V8 §11.2); the phone
/// does the same over its own few tones, so a section keeps one colour.
enum PlannerPalette {
    static let tones: [Color] = [
        Color(Theme.dynamic(dark: 0xC9A27A, light: 0x8A5F33)),
        Color(Theme.dynamic(dark: 0x9DB89A, light: 0x4C7350)),
        Color(Theme.dynamic(dark: 0xC79AA6, light: 0x8C4A5C)),
        Color(Theme.dynamic(dark: 0xA9A2D0, light: 0x5A528F)),
        Color(Theme.dynamic(dark: 0x8FBDBE, light: 0x3C7376)),
        Color(Theme.dynamic(dark: 0xCFC08A, light: 0x7C6A25)),
    ]

    static func color(for name: String) -> Color {
        var hash: UInt64 = 0xCBF2_9CE4_8422_2325
        for byte in name.trimmingCharacters(in: .whitespaces).lowercased().utf8 {
            hash ^= UInt64(byte)
            hash = hash &* 0x100_0000_01B3
        }
        return tones[Int(hash % UInt64(tones.count))]
    }
}

struct PlannerRow: View {
    @Environment(AppModel.self) private var model
    var item: PlannerItem
    var day: VaultDay
    @Binding var timing: PlannerItem?
    @Binding var editing: PlannerItem?

    private var timeText: String {
        guard let time = item.time else { return "" }
        func clock(_ minutes: Int) -> String { String(format: "%02d:%02d", minutes / 60, minutes % 60) }
        if let end = time.endMinutes {
            return "\(clock(time.startMinutes))–\(clock(end))"
        }
        return clock(time.startMinutes)
    }

    var body: some View {
        let faded = item.done || item.struck
        let row = HStack(alignment: .center, spacing: 10) {
            Text(timeText)
                .font(Theme.mono(12.5))
                .foregroundStyle(Theme.muted)
                .frame(width: 88, alignment: .leading)
            if item.isCalendar {
                RoundedRectangle(cornerRadius: 2).fill(Theme.cal).frame(width: 3, height: 22)
            }
            Checkbox(checked: item.done)
            Text(Rich.text(Inline.parse(item.label)))
                .font(.system(size: 17))
                .foregroundStyle(faded ? Theme.dim : Theme.ink)
                .strikethrough(faded, color: Theme.dim)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.vertical, 7)
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(timeText.isEmpty ? "" : timeText + ", ")\(Inline.plainText(item.label))")
        .accessibilityValue(item.done ? "Done" : "Not done")

        if item.isCalendar || model.isReadOnly {
            row.accessibilityHint(item.isCalendar ? "From your calendar" : "")
        } else {
            row
                .onTapGesture { model.tick(item, day: day) }
                .accessibilityAddTraits(.isButton)
                .contextMenu {
                    Button(item.done ? "Untick" : "Tick") { model.tick(item, day: day) }
                    Button("Set a time…") { timing = item }
                    Button("Edit this line") { editing = item }
                    Button("Move to Soon") {
                        if model.perform({ try $0.moveToSoon(item, day: day) }) {
                            model.show("Moved to Backlog · Soon")
                        }
                    }
                    Button("Remove", role: .destructive) { model.remove(item, day: day) }
                }
        }
    }
}

struct AddLineRow: View {
    @Environment(AppModel.self) private var model
    var group: PlannerGroup
    var day: VaultDay
    @State private var text = ""
    @State private var adding = false
    @FocusState private var focused: Bool

    var body: some View {
        HStack(spacing: 10) {
            Color.clear.frame(width: 88, height: 1)
            Text("+")
                .font(Theme.mono(17))
                .foregroundStyle(Theme.amber)
                .frame(width: 20)
            if adding {
                TextField("A line for the plan", text: $text)
                    .font(.system(size: 17))
                    .foregroundStyle(Theme.ink)
                    .focused($focused)
                    .submitLabel(.done)
                    .onSubmit(commit)
                    .onChange(of: focused) { _, isFocused in
                        if !isFocused {
                            commit()
                            adding = false
                        }
                    }
            } else {
                Text("Add a line")
                    .font(.system(size: 17))
                    .foregroundStyle(Theme.dim)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .padding(.vertical, 7)
        .contentShape(Rectangle())
        .onTapGesture {
            adding = true
            focused = true
        }
        .accessibilityLabel(group.name.map { "Add a line to \($0)" } ?? "Add a line")
        .accessibilityAddTraits(.isButton)
    }

    private func commit() {
        let line = text
        text = ""
        guard !line.trimmingCharacters(in: .whitespaces).isEmpty else { return }
        model.perform { try $0.addLine(line, group: group, day: day) }
    }
}
