import SwiftUI
import ThockKit

/// The backlog as a canvas (V38 §5): Soon and Someday with their categories,
/// a collapsed Done list, and one list the rows are dragged in. A drop, a
/// menu move and a tick are each one write of one task block.
struct BacklogScreen: View {
    @Environment(AppModel.self) private var model
    @State private var editing: BacklogTask?
    @State private var adding: BacklogGroup?
    @State private var showingDone = false
    @State private var collapsed: Set<String> = Self.loadCollapsed()

    /// One row of the list. Labels sit between the tasks so a drop can read
    /// its destination from the row above it.
    enum Row: Identifiable, Equatable {
        case section(BacklogSection)
        case category(BacklogGroup, BacklogSection)
        case task(BacklogTask, BacklogGroup)
        case empty(BacklogSection)
        case done(Int)
        case completed(BacklogTask)

        var id: String {
            switch self {
            case .section(let section): return "section-\(section.kind)"
            case .category(let group, _): return "category-\(group.id)"
            case .task(let task, _): return "task-\(task.line)"
            case .empty(let section): return "empty-\(section.kind)"
            case .done: return "done"
            case .completed(let task): return "completed-\(task.line)"
            }
        }

        var isTask: Bool {
            if case .task = self { return true }
            return false
        }
    }

    private static let collapsedKey = "backlog.collapsed"

    private static func loadCollapsed() -> Set<String> {
        Set(ThockEnvironment.defaults.stringArray(forKey: collapsedKey) ?? [])
    }

    private func key(_ group: BacklogGroup, in section: BacklogSection) -> String {
        "\(section.kind)/\(group.name ?? "")"
    }

    private func rows(_ view: BacklogView) -> [Row] {
        var rows: [Row] = []
        for section in view.openSections {
            rows.append(.section(section))
            let open = section.looseGroup.openTasks
            for task in open where !model.isBeingRemoved(task) {
                rows.append(.task(task, section.looseGroup))
            }
            if section.openCount == 0, section.categories.isEmpty {
                rows.append(.empty(section))
            }
            for group in section.categories {
                rows.append(.category(group, section))
                guard !collapsed.contains(key(group, in: section)) else { continue }
                for task in group.openTasks where !model.isBeingRemoved(task) {
                    rows.append(.task(task, group))
                }
            }
        }
        rows.append(.done(view.completedTasks.count))
        if showingDone {
            for task in view.completedTasks {
                rows.append(.completed(task))
            }
        }
        return rows
    }

    var body: some View {
        let _ = model.revision
        let view = model.session?.backlog() ?? BacklogView(text: "", config: VaultConfig())
        let rows = rows(view)
        VStack(alignment: .leading, spacing: 0) {
            header(view)
            List {
                ForEach(rows) { row in
                    rowView(row, view: view, rows: rows)
                        .listRowBackground(Theme.ground)
                        .listRowSeparator(.hidden)
                        .listRowInsets(EdgeInsets(top: 0, leading: 22, bottom: 0, trailing: 22))
                        .moveDisabled(!row.isTask || model.isReadOnly)
                }
                .onMove { from, to in
                    drop(rows: rows, from: from, to: to, view: view)
                }
                Color.clear.frame(height: 24)
                    .listRowBackground(Theme.ground)
                    .listRowSeparator(.hidden)
                    .moveDisabled(true)
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .environment(\.defaultMinListRowHeight, 1)
        }
        .background(Theme.ground)
        .sheet(item: $editing) { task in
            BacklogLineSheet(title: "Edit this task", initial: TaskLineParts(task.raw)?.text ?? task.label, hint: "Its checkbox and anything written under it stay as they are.") { text in
                model.backlogEdit(task, text: text)
            }
        }
        .sheet(item: $adding) { group in
            BacklogLineSheet(title: group.name.map { "Add to \($0)" } ?? "Add to \(view.section(group.heading))", initial: "", hint: "It lands at the end of the group.") { text in
                model.backlogAdd(text, to: group)
            }
        }
    }

    private func header(_ view: BacklogView) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Button {
                    model.closeBacklog()
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "chevron.left")
                            .font(.system(size: 14, weight: .semibold))
                        Text("Today")
                    }
                    .font(.system(size: 17))
                    .foregroundStyle(Theme.amber)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Back to today")
                Spacer()
                if !model.isReadOnly {
                    Button {
                        adding = view.soon.looseGroup
                    } label: {
                        Image(systemName: "plus")
                            .font(.system(size: 17, weight: .semibold))
                            .foregroundStyle(Theme.amber)
                            .frame(width: 44, height: 44)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Add a task to Soon")
                }
            }
            .padding(.top, 6)
            Text("Backlog")
                .font(Theme.serif(30, style: .title1))
                .foregroundStyle(Theme.ink)
            Text(Self.counts(view).uppercased())
                .font(Theme.label())
                .tracking(0.9)
                .foregroundStyle(Theme.dim)
                .padding(.bottom, 6)
        }
        .padding(.horizontal, 22)
    }

    static func counts(_ view: BacklogView) -> String {
        guard view.soon.exists || view.someday.exists else { return "empty" }
        return "\(view.soon.openCount) \(view.soon.title) · \(view.someday.openCount) \(view.someday.title)"
    }

    @ViewBuilder
    private func rowView(_ row: Row, view: BacklogView, rows: [Row]) -> some View {
        switch row {
        case .section(let section):
            HStack(alignment: .firstTextBaseline) {
                CardLabel(title: section.title, note: section.openCount == 0 ? nil : "\(section.openCount)")
                if !model.isReadOnly {
                    addButton(section.looseGroup, label: "Add a task to \(section.title)")
                }
            }
            .padding(.top, 18)
            .padding(.bottom, 6)
            .overlay(alignment: .bottom) { Hairline() }
            .accessibilityAddTraits(.isHeader)
        case .category(let group, let section):
            let key = key(group, in: section)
            let isCollapsed = collapsed.contains(key)
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Image(systemName: isCollapsed ? "chevron.right" : "chevron.down")
                    .font(.system(size: 10, weight: .bold))
                    .foregroundStyle(Theme.dim)
                Text(group.name ?? "")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(PlannerPalette.color(for: group.name ?? ""))
                Text("\(group.openTasks.count)")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Theme.dim)
                Spacer()
                if !model.isReadOnly {
                    addButton(group, label: "Add a task to \(group.name ?? "")")
                }
            }
            .padding(.top, 10)
            .padding(.bottom, 2)
            .contentShape(Rectangle())
            .onTapGesture { toggle(key) }
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isButton)
            .accessibilityHint(isCollapsed ? "Expands the group" : "Collapses the group")
        case .task(let task, let group):
            BacklogRow(task: task, group: group, view: view, rows: rows, editing: $editing)
        case .empty(let section):
            Text(section.exists ? "Nothing here yet." : "Nothing in \(section.title) yet.")
                .font(.system(size: 16))
                .foregroundStyle(Theme.dim)
                .padding(.vertical, 8)
        case .done(let count):
            Button {
                withAnimation(.snappy) { showingDone.toggle() }
            } label: {
                HStack(alignment: .firstTextBaseline) {
                    CardLabel(title: "Done", note: count == 0 ? nil : "\(count)")
                    Image(systemName: showingDone ? "chevron.down" : "chevron.right")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.dim)
                }
                .padding(.top, 22)
                .padding(.bottom, 6)
                .overlay(alignment: .bottom) { Hairline() }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Done, \(count)")
            .accessibilityHint(showingDone ? "Hides the completed tasks" : "Shows the completed tasks")
        case .completed(let task):
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Checkbox(checked: true, size: 18)
                    .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 3 }
                Text(Rich.text(Inline.parse(task.label)))
                    .font(.system(size: 17))
                    .foregroundStyle(Theme.dim)
                    .strikethrough(true, color: Theme.dim)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if let date = task.completedOn {
                    Text(date)
                        .font(Theme.mono(12))
                        .foregroundStyle(Theme.dim)
                }
            }
            .padding(.vertical, 7)
            .accessibilityElement(children: .combine)
        }
    }

    private func addButton(_ group: BacklogGroup, label: String) -> some View {
        Button {
            adding = group
        } label: {
            Text("+")
                .font(Theme.mono(15))
                .foregroundStyle(Theme.dim)
                .frame(width: 28, height: 28)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
    }

    private func toggle(_ key: String) {
        if collapsed.contains(key) {
            collapsed.remove(key)
        } else {
            collapsed.insert(key)
        }
        ThockEnvironment.defaults.set(Array(collapsed).sorted(), forKey: Self.collapsedKey)
    }

    /// Where a dragged row landed: the group and the row above it decide
    /// the write (V38 §6.2). A drop under a collapsed category goes to its
    /// end; a drop among the done tasks is refused.
    private func drop(rows: [Row], from: IndexSet, to: Int, view: BacklogView) {
        guard let source = from.first, case .task(let task, _) = rows[source] else { return }
        var moved = rows
        moved.move(fromOffsets: from, toOffset: to)
        guard let landed = moved.firstIndex(where: { $0.id == rows[source].id }), landed != source else { return }
        guard landed > 0 else { return }
        let destination: BacklogGroup
        let place: BacklogPlace
        switch moved[landed - 1] {
        case .task(let above, let group):
            destination = group
            place = .after(above)
        case .category(let group, let section):
            destination = group
            place = collapsed.contains(key(group, in: section)) ? .end : .top
        case .section(let section), .empty(let section):
            destination = section.looseGroup
            place = .top
        case .done, .completed:
            model.show("To finish a task, tick it.")
            return
        }
        model.backlogMove(task, to: destination, place: place)
    }
}

extension BacklogView {
    /// The section a group's heading belongs to, for a sheet's title.
    func section(_ heading: HeadingRef) -> String {
        if soon.groups.contains(where: { $0.heading == heading }) { return soon.title }
        if someday.groups.contains(where: { $0.heading == heading }) { return someday.title }
        return "Backlog"
    }
}

/// One open task: the circle ticks, the words edit, a press-and-move anywhere
/// on the row drags it, and a long press opens the menu with every move (V38 §5.2).
struct BacklogRow: View {
    @Environment(AppModel.self) private var model
    var task: BacklogTask
    var group: BacklogGroup
    var view: BacklogView
    var rows: [BacklogScreen.Row]
    @Binding var editing: BacklogTask?

    /// The open tasks drawn in this group, in order.
    private var siblings: [BacklogTask] {
        rows.compactMap { row in
            if case .task(let other, let otherGroup) = row, otherGroup.id == group.id { return other }
            return nil
        }
    }

    var body: some View {
        let editable = !model.isReadOnly
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Button {
                model.backlogTick(task)
            } label: {
                Circle()
                    .stroke(Theme.muted, lineWidth: 1.5)
                    .frame(width: 20, height: 20)
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .disabled(!editable)
            .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 3 }
            .accessibilityLabel("Tick")
            Text(Rich.text(Inline.parse(task.label)))
                .font(.system(size: 17))
                .foregroundStyle(Theme.ink)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
                .onTapGesture {
                    if editable { editing = task }
                }
            if task.childLines > 0 {
                Text("+\(task.childLines) \(task.childLines == 1 ? "line" : "lines")")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Theme.dim)
            }
        }
        .padding(.vertical, 7)
        .padding(.leading, group.isCategory ? 10 : 0)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Inline.plainText(task.label))
        .accessibilityValue(group.name.map { "in \($0)" } ?? "")
        .contextMenu {
            if editable {
                Button("Tick") { model.backlogTick(task) }
                Button("Edit this task") { editing = task }
                moveUpDown
                Menu("Move to…") {
                    ForEach(view.openSections, id: \.kind) { section in
                        Button(section.title) { model.backlogMove(task, from: group, toSection: section) }
                        ForEach(section.categories) { category in
                            if category.id != group.id {
                                Button("\(section.title) › \(category.name ?? "")") { model.backlogMove(task, to: category, place: .end) }
                            }
                        }
                    }
                }
                Button("Move to today") { model.backlogMoveToToday(task) }
                Button("Remove", role: .destructive) { model.backlogRemove(task) }
            }
        }
    }

    @ViewBuilder
    private var moveUpDown: some View {
        let siblings = siblings
        let position = siblings.firstIndex(of: task) ?? 0
        if position > 0 {
            Button("Move up") {
                model.backlogMove(task, to: group, place: position == 1 ? .top : .after(siblings[position - 2]))
            }
        }
        if position + 1 < siblings.count {
            Button("Move down") {
                model.backlogMove(task, to: group, place: .after(siblings[position + 1]))
            }
        }
    }
}

/// The Backlog row on today's canvas (V38 §4): the counts, and the way in.
struct BacklogRowOnToday: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let _ = model.revision
        let view = model.session?.backlog()
        let counts = view.map(BacklogScreen.counts) ?? "empty"
        Button {
            model.openBacklog()
        } label: {
            VStack(alignment: .leading, spacing: 0) {
                Hairline()
                HStack(alignment: .firstTextBaseline) {
                    Text("BACKLOG")
                        .font(Theme.label())
                        .tracking(1.1)
                        .foregroundStyle(Theme.dim)
                    Spacer()
                    Text(counts)
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(Theme.dim)
                    Image(systemName: "chevron.right")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.dim)
                }
                .padding(.top, 10)
                .padding(.bottom, 2)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Backlog, \(counts)")
    }
}

/// Editing or adding one task: the words only.
struct BacklogLineSheet: View {
    @Environment(\.dismiss) private var dismiss
    var title: String
    var initial: String
    var hint: String
    var onDone: (String) -> Void

    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            SheetHeader(leading: "Cancel", title: title, trailing: "Done", trailingEnabled: !text.trimmingCharacters(in: .whitespaces).isEmpty) {
                dismiss()
            } onTrailing: {
                onDone(text)
                dismiss()
            }
            TextField("The task", text: $text, axis: .vertical)
                .font(.system(size: 19))
                .foregroundStyle(Theme.ink)
                .lineLimit(1...4)
                .focused($focused)
            Text(hint)
                .font(.system(size: 13))
                .foregroundStyle(Theme.dim)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 20)
        .presentationDetents([.medium])
        .presentationBackground(Theme.surface)
        .presentationCornerRadius(26)
        .onAppear {
            text = initial
            focused = true
        }
    }
}
