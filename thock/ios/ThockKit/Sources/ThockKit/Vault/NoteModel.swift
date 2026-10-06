import Foundation

public enum CardKind: Equatable, Sendable {
    /// Text between the note's title and its first section.
    case preamble
    case journal
    case planner
    /// A level-1 heading after the user's sections: the agent's voice.
    case agent
    case prose
}

/// One section of a note, drawn as a card on the Today canvas. The cards are
/// whatever the note's own headings say (V33 §5).
public struct NoteCard: Identifiable, Equatable, Sendable {
    public var id: Int
    public var kind: CardKind
    public var title: String
    public var heading: HeadingRef?
    public var blocks: [Block]
}

public struct PlannerItem: Identifiable, Equatable, Sendable {
    public var id: Int { line }
    public var line: Int
    public var raw: String
    public var done: Bool
    public var time: TimePrefix?
    /// The words of the task, without checkbox, time or trailing markers.
    public var label: String
    /// The whole task was crossed out at the desk.
    public var struck: Bool
    /// Calendar lines are owned by the desk's calendar sync and read-only.
    public var isCalendar: Bool
    public var hash: String
    /// Position among the planner's lines with the same hash.
    public var ordinal: Int
    public var indent: String
    public var marker: Character
}

public struct PlannerGroup: Identifiable, Equatable, Sendable {
    public var id: Int
    /// `nil` for the lines above the first subsection.
    public var name: String?
    public var heading: HeadingRef?
    public var items: [PlannerItem]
    /// Plain text written among the tasks, such as a capture that was not a
    /// checkbox. Drawn below the group's tasks, never ticked.
    public var notes: [Block] = []

    public var isCalendar: Bool {
        name.map { SyncCore.headingKey($0) == "calendar" } ?? false
    }
}

public struct Planner: Equatable, Sendable {
    public var heading: HeadingRef
    /// Whether the heading exists in the note yet.
    public var exists: Bool
    public var groups: [PlannerGroup]

    public var items: [PlannerItem] { groups.flatMap(\.items) }
    public var doneCount: Int { items.filter(\.done).count }
}

public struct JournalEntry: Identifiable, Equatable, Sendable {
    public var id: Int { line }
    public var line: Int
    /// `21:14` when the paragraph starts with the phone's `**21:14** · `.
    public var time: String?
    /// The paragraph's inline Markdown after the time prefix.
    public var text: String
    public var source: [String]
    /// The template's italic prompt, shown only while the journal is empty.
    public var isPrompt: Bool
}

public struct Journal: Equatable, Sendable {
    public var heading: HeadingRef
    public var exists: Bool
    public var entries: [JournalEntry]
    /// Everything in the section in order, entries and the blocks between
    /// them, for drawing the day's thread.
    public var blocks: [Block]

    public var written: [JournalEntry] { entries.filter { !$0.isPrompt } }
}

/// A daily or weekly note read for display. Nothing here is written back;
/// every change goes through a write (`PhoneWrites`).
public struct NoteView: Equatable, Sendable {
    public var kind: NoteKind
    public var title: String?
    public var cards: [NoteCard]
    /// The day's planner, or the week's goals: the checklist the nudges act on.
    public var planner: Planner
    public var journal: Journal

    public init(text: String, config: VaultConfig, kind: NoteKind = .daily) {
        self.kind = kind
        let file = TextFile(text)
        let lines = file.lines.map(\.text)
        let headings = file.headings()
        // A week has goals where a day has its planner, and no journal.
        let checklistNames = kind == .weekly ? config.goalsHeadings : config.plannerHeadings
        let plannerHeading = file.resolve(names: checklistNames)
        let journalHeading = kind == .weekly ? nil : file.resolve(names: config.journalHeadings)

        var titleHeading: HeadingLine?
        if let first = headings.first, first.level == 1 {
            titleHeading = first
            title = first.text
        }

        let afterTitle = headings.filter { $0.index != titleHeading?.index }
        let titleEnd = titleHeading.map { file.section(of: $0).end } ?? lines.count
        let inside = afterTitle.filter { $0.index < titleEnd }
        let cardLevel = inside.map(\.level).min() ?? 2
        var cardHeadings = inside.filter { $0.level == cardLevel }
        // Past the title's section only another level-1 heading (the agent's
        // voice) starts a card; its subheadings are part of it.
        cardHeadings += afterTitle.filter { $0.index >= titleEnd && $0.level == 1 }
        // The planner and journal draw their own subsections (the desk's
        // `Calendar` among them), so those never become cards of their own,
        // whatever level the note puts them at.
        let owned = [plannerHeading, journalHeading].compactMap { $0 }.map { heading in
            let section = file.section(of: heading)
            return section.start..<section.end
        }
        cardHeadings.removeAll { heading in owned.contains { $0.contains(heading.index) } }
        cardHeadings.sort { $0.index < $1.index }

        var cards: [NoteCard] = []
        let preambleStart = (titleHeading?.index ?? -1) + 1
        let preambleEnd = cardHeadings.first?.index ?? lines.count
        if preambleStart < preambleEnd {
            let blocks = Blocks.parse(lines: Array(lines[preambleStart..<preambleEnd]), firstLine: preambleStart, alreadyInsideNote: preambleStart > 0)
            if blocks.contains(where: { $0.kind != .blank && $0.kind != .rule }) {
                cards.append(NoteCard(id: preambleStart, kind: .preamble, title: "", heading: nil, blocks: blocks))
            }
        }
        for (position, heading) in cardHeadings.enumerated() {
            let end = position + 1 < cardHeadings.count ? cardHeadings[position + 1].index : lines.count
            let kind: CardKind
            if heading.index == plannerHeading?.index {
                kind = .planner
            } else if heading.index == journalHeading?.index {
                kind = .journal
            } else if heading.level == 1, titleHeading != nil {
                kind = .agent
            } else {
                kind = .prose
            }
            let blocks = Blocks.parse(lines: Array(lines[(heading.index + 1)..<end]), firstLine: heading.index + 1, alreadyInsideNote: true)
            cards.append(NoteCard(id: heading.index, kind: kind, title: Inline.plainText(heading.text), heading: Self.reference(heading, in: headings), blocks: blocks))
        }
        self.cards = cards
        self.planner = Self.planner(file: file, heading: plannerHeading, headings: headings, names: checklistNames)
        self.journal = Self.journal(file: file, heading: journalHeading, headings: headings, config: config)
    }

    /// The paragraphs the phone may replace in place (V37 §6, §8): those of
    /// the user's own prose sections. The preamble, the agent's sections and
    /// anything outside the subset stay as the desk wrote them.
    public func isEditable(_ block: Block, in card: NoteCard) -> Bool {
        card.kind == .prose && block.kind == .paragraph && !Self.isPrompt(block)
    }

    /// The template's italic hint, left in the file for the desk and never
    /// replaced from the phone: a tap on it starts a new paragraph instead
    /// (V37 §11 #10).
    public static func isPrompt(_ block: Block) -> Bool {
        block.kind == .paragraph && isItalicOnly(block.text)
    }

    /// Names a heading the way a write must: its own text, and which of the
    /// identically named headings it is.
    static func reference(_ heading: HeadingLine, in headings: [HeadingLine]) -> HeadingRef {
        let lowered = heading.text.lowercased()
        let twins = headings.filter { $0.text.lowercased() == lowered }
        let ordinal = twins.firstIndex { $0.index == heading.index } ?? 0
        return HeadingRef(text: heading.text, level: heading.level, ordinal: ordinal)
    }

    static func planner(file: TextFile, heading: HeadingLine?, headings: [HeadingLine], names: [String]) -> Planner {
        guard let heading else {
            let reference = HeadingRef(text: names.first ?? "Day planner", level: 2)
            return Planner(heading: reference, exists: false, groups: [PlannerGroup(id: -1, name: nil, heading: reference, items: [])])
        }
        let reference = reference(heading, in: headings)
        let section = file.section(of: heading)
        let mask = file.contentMask()
        var groups = [PlannerGroup(id: heading.index, name: nil, heading: reference, items: [])]
        var seen: [String: Int] = [:]
        for index in section.start..<section.end where mask[index] {
            let text = file.lines[index].text
            // Ordinals count every line the applier would consider, not only
            // tasks: `- Call Ana` above `- [ ] Call Ana` shares its hash, and
            // a tick must not land on the plain bullet.
            let hash = SyncCore.lineHash(text)
            var ordinal = 0
            if section.body.contains(index), !SyncCore.lineIdentity(text).isEmpty {
                ordinal = seen[hash, default: 0]
                seen[hash] = ordinal + 1
            }
            if let child = TextFile.heading(in: text) {
                // Only headings one level down start a group; deeper ones
                // belong to the group they sit in, as on the desk.
                if child.level == heading.level + 1 {
                    let line = HeadingLine(index: index, level: child.level, text: child.text)
                    groups.append(PlannerGroup(id: index, name: Inline.plainText(child.text), heading: Self.reference(line, in: headings), items: []))
                }
                continue
            }
            guard let item = Blocks.listItem(text), case .task(let done) = item.kind else { continue }
            let withoutComment = Self.strippingTrailingComments(item.text)
            var time = TimePrefix.parse(withoutComment)
            var label = time.map { String($0.rest) } ?? withoutComment
            var struck = false
            // A crossed-out task may wrap its time (`~~09:00 Meeting~~`) or
            // only its words (`09:00 ~~Meeting~~`); either way it keeps its hour.
            let trimmed = label.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("~~"), trimmed.hasSuffix("~~"), trimmed.count > 4, !trimmed.dropFirst(2).dropLast(2).contains("~~") {
                struck = true
                label = String(trimmed.dropFirst(2).dropLast(2))
                if time == nil, let inner = TimePrefix.parse(label) {
                    time = inner
                    label = String(inner.rest)
                }
            }
            // An empty checkbox is a slot the template left to be filled at
            // the desk. It has no words for a write to name it by, so it is
            // not a line the phone can tick, edit or remove.
            guard !SyncCore.lineIdentity(text).isEmpty, !label.trimmingCharacters(in: .whitespaces).isEmpty else { continue }
            let isCalendar = text.contains("<!--gcal:") || groups[groups.count - 1].isCalendar
            let marker = text.trimmingLeadingWhitespace().first ?? "-"
            groups[groups.count - 1].items.append(PlannerItem(
                line: index, raw: text, done: done, time: time,
                label: label.trimmingCharacters(in: .whitespaces), struck: struck, isCalendar: isCalendar,
                hash: hash, ordinal: ordinal, indent: item.indent, marker: marker))
        }
        for position in groups.indices {
            let start = position == 0 ? section.start : groups[position].id + 1
            let end = position + 1 < groups.count ? groups[position + 1].id : section.end
            guard start < end else { continue }
            let lines = file.lines[start..<end].map(\.text)
            groups[position].notes = Blocks.parse(lines: lines, firstLine: start, alreadyInsideNote: true).filter(Self.isPlannerNote)
        }
        return Planner(heading: reference, exists: true, groups: groups)
    }

    /// Paragraphs, quotes and top-level bullets. The template's italic hint
    /// stays out, as do a task's indented continuation lines, which belong to
    /// the task rather than standing on their own.
    static func isPlannerNote(_ block: Block) -> Bool {
        switch block.kind {
        case .paragraph: return !isPrompt(block)
        case .quote: return true
        case .bullet, .numbered: return block.indent.isEmpty
        default: return false
        }
    }

    static func strippingTrailingComments(_ text: String) -> String {
        var output = text
        while true {
            output = output.trimmingTrailingWhitespace()
            guard output.hasSuffix("-->"), let open = output.range(of: "<!--", options: .backwards) else { return output }
            output = String(output[..<open.lowerBound])
        }
    }

    static func journal(file: TextFile, heading: HeadingLine?, headings: [HeadingLine], config: VaultConfig) -> Journal {
        guard let heading else {
            return Journal(heading: HeadingRef(text: config.journalHeadings.first ?? "Journal", level: 2), exists: false, entries: [], blocks: [])
        }
        let section = file.section(of: heading)
        let lines = file.lines[section.own].map(\.text)
        let blocks = Blocks.parse(lines: lines, firstLine: section.start, alreadyInsideNote: true)
        var entries: [JournalEntry] = []
        for block in blocks where block.kind == .paragraph {
            let (time, text) = Self.splitTimestamp(block.text)
            let isPrompt = entries.isEmpty && time == nil && Self.isItalicOnly(block.text)
            entries.append(JournalEntry(line: block.line, time: time, text: text, source: block.source, isPrompt: isPrompt))
        }
        return Journal(heading: reference(heading, in: headings), exists: true, entries: entries, blocks: blocks)
    }

    /// `**21:14** · words` → (`21:14`, `words`).
    public static func splitTimestamp(_ text: String) -> (String?, String) {
        guard text.hasPrefix("**") else { return (nil, text) }
        let rest = text.dropFirst(2)
        guard let close = rest.range(of: "**") else { return (nil, text) }
        let stamp = rest[..<close.lowerBound]
        guard let time = TimePrefix.parse(stamp), time.endMinutes == nil, time.rest.isEmpty else { return (nil, text) }
        var after = rest[close.upperBound...].trimmingLeadingWhitespace()
        if after.hasPrefix("·") {
            after = after.dropFirst().trimmingLeadingWhitespace()
        }
        return (String(stamp), String(after))
    }

    static func isItalicOnly(_ text: String) -> Bool {
        let runs = Inline.parse(text.replacingOccurrences(of: "\n", with: " ")).filter { !$0.comment }
        return !runs.isEmpty && runs.allSatisfy { $0.italic || $0.text.allSatisfy(\.isWhitespace) || $0.code }
    }
}

/// A capture's state, read from the vault on each refresh (V33 §6.3).
public enum ReceiptState: Equatable, Sendable {
    case waiting
    case filed(destination: String, day: VaultDay?)
    case discarded(day: VaultDay?)
    case addedToToday
    case addedToBacklog
    case gone
}

public struct TriageLogLine: Equatable, Sendable {
    public var day: VaultDay?
    public var title: String
    public var destination: String
    public var digest: String?
}

public enum TriageLog {
    /// `- 2026-08-23 · Title → Backlog · Someday <!--inbox:4d1f9a02c7b3-->`
    /// (V13 §9.5). Lines that do not fit are skipped: the log is written by a
    /// ritual and may be hand-edited.
    public static func parse(_ text: String) -> [TriageLogLine] {
        TextFile(text).lines.compactMap { parseLine($0.text) }
    }

    public static func parseLine(_ line: String) -> TriageLogLine? {
        var rest = line.trimmingCharacters(in: .whitespaces)
        guard rest.hasPrefix("- ") else { return nil }
        rest = String(rest.dropFirst(2))
        var digest: String?
        if rest.hasSuffix("-->"), let open = rest.range(of: "<!--inbox:", options: .backwards) {
            digest = String(rest[open.upperBound...].dropLast(3)).trimmingCharacters(in: .whitespaces)
            rest = String(rest[..<open.lowerBound]).trimmingCharacters(in: .whitespaces)
        }
        guard let dot = rest.range(of: " · ") else { return nil }
        let day = VaultDay(iso: String(rest[..<dot.lowerBound]))
        guard day != nil else { return nil }
        let body = rest[dot.upperBound...]
        guard let arrow = body.range(of: " → ", options: .backwards) else { return nil }
        let title = body[..<arrow.lowerBound].trimmingCharacters(in: .whitespaces)
        let destination = body[arrow.upperBound...].trimmingCharacters(in: .whitespaces)
        guard !destination.isEmpty else { return nil }
        return TriageLogLine(day: day, title: title, destination: destination, digest: digest)
    }
}

/// The frontmatter of an inbox note (V13 §6), as far as the phone reads it.
public struct InboxNote: Equatable, Sendable {
    public var path: String
    public var title: String
    public var source: String?
    public var digest: String?
    public var captured: String?
    public var url: String?

    public init(path: String, content: String) {
        self.path = path
        var fields: [String: String] = [:]
        let lines = TextFile(content).lines.map(\.text)
        var bodyStart = 0
        if lines.first?.trimmingTrailingWhitespace() == "---" {
            var index = 1
            while index < lines.count {
                let line = lines[index]
                index += 1
                let trimmed = line.trimmingTrailingWhitespace()
                if trimmed == "---" || trimmed == "..." { break }
                if let colon = line.firstIndex(of: ":") {
                    fields[line[..<colon].trimmingCharacters(in: .whitespaces)] = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
                }
            }
            bodyStart = index
        }
        source = fields["source"]
        digest = fields["capture"]
        captured = fields["captured"]
        url = fields["url"]
        let body = lines[min(bodyStart, lines.count)...].first { !$0.trimmingCharacters(in: .whitespaces).isEmpty } ?? ""
        let heading = TextFile.heading(in: body)
        // The phone edits a waiting note's heading but cannot reach its front
        // matter, so a level-1 heading is the newer of the two.
        if let heading, heading.level == 1, !heading.text.isEmpty {
            self.title = Inline.plainText(heading.text)
        } else if let title = fields["title"], !title.isEmpty {
            self.title = title
        } else {
            let first = heading?.text ?? body.trimmingCharacters(in: .whitespaces)
            let stem = path.split(separator: "/").last.map(String.init)?.replacingOccurrences(of: ".md", with: "") ?? path
            self.title = first.isEmpty ? stem : Inline.plainText(first)
        }
    }
}
