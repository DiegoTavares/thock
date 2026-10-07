import Foundation

/// One task of `backlog.md` as the Backlog screen draws it (V38 §5). A task
/// is addressed the way every phone write addresses a line: the group it
/// sits in, the hash of its words, and its ordinal among same-hash lines
/// of that group.
public struct BacklogTask: Identifiable, Equatable, Sendable {
    public var id: Int { line }
    public var line: Int
    public var raw: String
    /// The words, without checkbox, completion stamp or trailing markers.
    public var label: String
    public var done: Bool
    /// `2026-10-01` from a ` ✅ 2026-10-01` suffix, on a completed task.
    public var completedOn: String?
    /// The group a write to this task names: its category, or its section
    /// when it is loose.
    public var heading: HeadingRef
    public var hash: String
    public var ordinal: Int
    /// The task line and its indented continuation, which travel together.
    public var block: [String]

    public var childLines: Int { block.count - 1 }
}

public struct BacklogGroup: Identifiable, Equatable, Sendable {
    public var id: Int
    /// `nil` for the loose tasks above a section's first category.
    public var name: String?
    public var heading: HeadingRef
    public var level: Int
    public var tasks: [BacklogTask]

    public var isCategory: Bool { name != nil }
    public var openTasks: [BacklogTask] { tasks.filter { !$0.done } }
}

public struct BacklogSection: Equatable, Sendable {
    public enum Kind: Equatable, Sendable {
        case soon
        case someday
        case completed
    }

    public var kind: Kind
    /// The heading as the file spells it, or the configured name when the
    /// section is missing.
    public var title: String
    public var heading: HeadingRef
    public var exists: Bool
    /// The loose group first, then one group per category in file order.
    public var groups: [BacklogGroup]

    public var tasks: [BacklogTask] { groups.flatMap(\.tasks) }
    public var openCount: Int { tasks.filter { !$0.done }.count }
    public var looseGroup: BacklogGroup { groups[0] }
    public var categories: [BacklogGroup] { Array(groups.dropFirst()) }
}

/// `backlog.md` read for display: Soon, Someday and Completed by the vault's
/// configured headings, categories as the desk reads them (V17 §4). Nothing
/// here is written back; every change goes through a write (`PhoneWrites`).
public struct BacklogView: Equatable, Sendable {
    public var soon: BacklogSection
    public var someday: BacklogSection
    public var completed: BacklogSection

    public init(text: String, config: VaultConfig) {
        let file = TextFile(text)
        soon = Self.section(.soon, names: [config.soonHeading, "Soon"], file: file)
        someday = Self.section(.someday, names: [config.somedayHeading, "Someday"], file: file)
        completed = Self.section(.completed, names: [config.completedHeading, "Completed"], file: file)
    }

    public var openSections: [BacklogSection] { [soon, someday] }

    /// Completed, newest first; a hand-written completion with no date
    /// sorts last, as on the desk.
    public var completedTasks: [BacklogTask] {
        let dated = completed.tasks.filter { $0.completedOn != nil }.reversed()
        let undated = completed.tasks.filter { $0.completedOn == nil }
        return dated.sorted { ($0.completedOn ?? "") > ($1.completedOn ?? "") } + undated
    }

    public func section(_ kind: BacklogSection.Kind) -> BacklogSection {
        switch kind {
        case .soon: return soon
        case .someday: return someday
        case .completed: return completed
        }
    }

    /// The open group a task belongs to, by its heading.
    public func group(of task: BacklogTask) -> BacklogGroup? {
        openSections.flatMap(\.groups).first { $0.heading == task.heading && $0.tasks.contains(task) }
    }

    public func section(of task: BacklogTask) -> BacklogSection? {
        [soon, someday, completed].first { $0.tasks.contains(task) }
    }

    static func section(_ kind: BacklogSection.Kind, names: [String], file: TextFile) -> BacklogSection {
        let headings = file.headings()
        guard let heading = file.resolve(names: names) else {
            let reference = HeadingRef(text: names[0], level: 2)
            return BacklogSection(kind: kind, title: names[0], heading: reference, exists: false,
                                  groups: [BacklogGroup(id: -1, name: nil, heading: reference, level: 2, tasks: [])])
        }
        let reference = NoteView.reference(heading, in: headings)
        let range = file.section(of: heading)
        var groups = [BacklogGroup(id: heading.index, name: nil, heading: reference, level: heading.level, tasks: [])]
        var own = own(of: range)
        for index in range.start..<range.end {
            if let child = headings.first(where: { $0.index == index }) {
                // Any deeper heading starts a category and ends the one
                // before it; there is no nesting, as on the desk.
                let childRange = file.section(of: child)
                groups.append(BacklogGroup(id: index, name: Inline.plainText(child.text), heading: NoteView.reference(child, in: headings), level: child.level, tasks: []))
                own = Self.own(of: childRange)
                continue
            }
            guard own.contains(index), let task = task(at: index, in: file, own: own, heading: groups[groups.count - 1].heading) else { continue }
            groups[groups.count - 1].tasks.append(task)
        }
        return BacklogSection(kind: kind, title: heading.text, heading: reference, exists: true, groups: groups)
    }

    /// A group's own lines: under its heading, above the next heading, the
    /// range a block write searches (V38 §7.1).
    private static func own(of range: SectionRange) -> Range<Int> {
        range.start..<range.ownEnd
    }

    private static func task(at index: Int, in file: TextFile, own: Range<Int>, heading: HeadingRef) -> BacklogTask? {
        let mask = file.contentMask()
        guard mask[index] else { return nil }
        let text = file.lines[index].text
        guard let item = Blocks.listItem(text), item.indent.isEmpty, case .task(let done) = item.kind else { return nil }
        let identity = SyncCore.lineIdentity(text)
        guard !identity.isEmpty else { return nil }
        let hash = SyncCore.lineHash(text)
        let ordinal = own.filter { $0 < index && mask[$0] && SyncCore.lineHash(file.lines[$0].text) == hash && !SyncCore.lineIdentity(file.lines[$0].text).isEmpty }.count
        var label = NoteView.strippingTrailingComments(item.text).trimmingCharacters(in: .whitespaces)
        var completedOn: String?
        if let range = label.range(of: #"\s*✅\s*(\d{4}-\d{2}-\d{2})\s*$"#, options: .regularExpression) {
            completedOn = String(label[range]).components(separatedBy: "✅").last?.trimmingCharacters(in: .whitespaces)
            label = String(label[..<range.lowerBound])
        }
        var section = SectionRange(headingIndex: nil, level: 0, start: own.lowerBound, end: own.upperBound, bodyEnd: own.upperBound, ownEnd: own.upperBound)
        section.bodyEnd = own.upperBound
        let end = SyncCore.blockEnd(from: index, in: section, file: file)
        return BacklogTask(line: index, raw: text, label: label, done: done, completedOn: completedOn, heading: heading,
                           hash: hash, ordinal: ordinal, block: file.lines[index..<end].map(\.text))
    }
}

/// Where a moved task lands in its destination group.
public enum BacklogPlace: Equatable, Sendable {
    case top
    case end
    case after(BacklogTask)
}

extension PhoneWrites {
    // MARK: Backlog (V38 §6)

    /// Moves a task into a group: a reorder, a change of category, or a
    /// change of section, as one `move_block`.
    public func backlogMove(_ task: BacklogTask, to group: BacklogGroup, place: BacklogPlace) -> PlannedWrite {
        var write = document(.moveBlock, path: config.backlogFile)
        write.heading = task.heading
        write.lineHash = task.hash
        write.ordinal = task.ordinal
        write.to = group.heading
        switch place {
        case .top: write.place = .top
        case .end: write.place = .end
        case .after(let anchor): write.place = .after(lineHash: anchor.hash, ordinal: anchor.ordinal)
        }
        return PlannedWrite(document: write)
    }

    /// The menu's section items, the desk's chevron (V17 §5.3): a
    /// categorized task keeps its category, recreated under the destination
    /// when missing; a loose task lands loose.
    public func backlogMove(_ task: BacklogTask, from group: BacklogGroup, toSection section: BacklogSection) -> PlannedWrite {
        var write = document(.moveBlock, path: config.backlogFile)
        write.heading = task.heading
        write.lineHash = task.hash
        write.ordinal = task.ordinal
        write.place = .end
        if let name = group.name {
            write.to = HeadingRef(text: name, level: group.level)
            write.createUnder = section.heading
        } else {
            write.to = section.heading
        }
        return PlannedWrite(document: write)
    }

    /// Marks a task done (V6 §6.3): today's note records it first, then the
    /// block moves to the end of Completed with its stamp.
    public func backlogTick(_ task: BacklogTask, completed: BacklogSection, todayNote: String?, template: String?) -> [PlannedWrite] {
        guard var parts = TaskLineParts(task.raw) else { return [] }
        let view = NoteView(text: todayNote ?? template.map { Template.expand($0, day: today, time: clock, title: today.formatted(config.daily.filename)) } ?? "", config: config)
        var record = document(.append, path: config.dailyPath(today))
        record.heading = view.planner.heading
        record.lines = ["- [x] " + parts.text.trimmingCharacters(in: .whitespaces)]
        record.placement = .beforeChildren
        record.createFromTemplate = true

        parts.text = parts.text.trimmingCharacters(in: .whitespaces) + " ✅ " + today.iso
        var move = document(.moveBlock, path: config.backlogFile)
        move.heading = task.heading
        move.lineHash = task.hash
        move.ordinal = task.ordinal
        move.to = completed.heading
        move.place = .end
        move.newLine = parts.line(done: true)
        return [PlannedWrite(document: record, seed: dailySeed(today)), PlannedWrite(document: move)]
    }

    public func backlogEdit(_ task: BacklogTask, text: String) -> PlannedWrite? {
        guard var parts = TaskLineParts(task.raw) else { return nil }
        let cleaned = text.split(whereSeparator: \.isNewline).joined(separator: " ").trimmingCharacters(in: .whitespaces)
        guard !cleaned.isEmpty, cleaned != parts.text.trimmingCharacters(in: .whitespaces) else { return nil }
        parts.text = cleaned
        var write = document(.replaceLine, path: config.backlogFile)
        write.heading = task.heading
        write.lineHash = task.hash
        write.ordinal = task.ordinal
        write.newLine = parts.line()
        return PlannedWrite(document: write)
    }

    /// A new task at the end of a group: below a section's loose tasks and
    /// above its categories, or at the end of a category.
    public func backlogAdd(_ text: String, to group: BacklogGroup) -> PlannedWrite? {
        let cleaned = text.split(whereSeparator: \.isNewline).joined(separator: " ").trimmingCharacters(in: .whitespaces)
        guard !cleaned.isEmpty else { return nil }
        var write = document(.append, path: config.backlogFile)
        write.heading = group.heading
        write.lines = ["- [ ] " + cleaned]
        write.placement = group.isCategory ? .end : .beforeChildren
        return PlannedWrite(document: write)
    }

    public func backlogRemove(_ task: BacklogTask) -> PlannedWrite {
        var write = document(.removeBlock, path: config.backlogFile)
        write.heading = task.heading
        write.lineHash = task.hash
        write.ordinal = task.ordinal
        return PlannedWrite(document: write)
    }

    /// The desk's `t`: the block goes to today's plan as it is, then leaves
    /// the backlog. The planner's Move to Soon, in reverse.
    public func backlogMoveToToday(_ task: BacklogTask, todayNote: String?, template: String?) -> [PlannedWrite] {
        let view = NoteView(text: todayNote ?? template.map { Template.expand($0, day: today, time: clock, title: today.formatted(config.daily.filename)) } ?? "", config: config)
        var append = document(.append, path: config.dailyPath(today))
        append.heading = view.planner.heading
        append.lines = task.block
        append.placement = .beforeChildren
        append.createFromTemplate = true
        return [PlannedWrite(document: append, seed: dailySeed(today)), backlogRemove(task)]
    }
}

extension VaultSession {
    /// The backlog as the screen draws it. A vault without the file is an
    /// empty backlog, never an error (V38 §5.1).
    public func backlog() -> BacklogView {
        BacklogView(text: store.content(config.backlogFile) ?? "", config: config)
    }

    public func moveBacklog(_ task: BacklogTask, to group: BacklogGroup, place: BacklogPlace) throws {
        try record([writes().backlogMove(task, to: group, place: place)])
    }

    public func moveBacklog(_ task: BacklogTask, from group: BacklogGroup, toSection section: BacklogSection) throws {
        try record([writes().backlogMove(task, from: group, toSection: section)])
    }

    public func tickBacklog(_ task: BacklogTask, now: Date = Date()) throws {
        let builder = writes(now: now)
        let backlog = backlog()
        try record(builder.backlogTick(task, completed: backlog.completed, todayNote: noteText(builder.today), template: store.content(config.daily.template)))
    }

    public func editBacklog(_ task: BacklogTask, text: String) throws {
        guard let write = writes().backlogEdit(task, text: text) else { return }
        try record([write])
    }

    public func addBacklog(_ text: String, to group: BacklogGroup) throws {
        guard let write = writes().backlogAdd(text, to: group) else { return }
        try record([write])
    }

    public func removeBacklog(_ task: BacklogTask) throws {
        try record([writes().backlogRemove(task)])
    }

    public func moveBacklogToToday(_ task: BacklogTask, now: Date = Date()) throws {
        let builder = writes(now: now)
        try record(builder.backlogMoveToToday(task, todayNote: noteText(builder.today), template: store.content(config.daily.template)))
    }
}
