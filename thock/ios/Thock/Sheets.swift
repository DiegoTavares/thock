import SwiftUI
import ThockKit

/// Clip from inside the app: the same sheet as the share extension, starting
/// from a pasted link.
struct InAppClipSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ClipSheet(draft: ClipDraft(), asksForLink: true) {
            dismiss()
        } onSave: { clip in
            if model.perform({ try $0.clip(clip) }) {
                UINotificationFeedbackGenerator().notificationOccurred(.success)
                model.show(clip.article == nil ? "Link saved to your inbox" : "Saved to your inbox, text kept")
            }
            dismiss()
        }
    }
}

/// The capture feed behind the Inbox row: everything this phone caught, and
/// what became of it (V33 §6.3).
struct ReceiptsScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let _ = model.revision
        let receipts = model.store?.receipts() ?? []
        let mine = Set(receipts.compactMap(\.record.inboxPath))
        let others = (model.store?.waitingInboxNotes() ?? []).filter { !mine.contains($0.path) }
        VStack(alignment: .leading, spacing: 0) {
            SheetHeader(leading: "", title: "", trailing: "Done", onLeading: {}, onTrailing: { dismiss() })
                .padding(.bottom, 4)
            VStack(alignment: .leading, spacing: 4) {
                Text("CAPTURES")
                    .font(Theme.label())
                    .tracking(0.9)
                    .foregroundStyle(Theme.dim)
                Text("Everything you caught")
                    .font(Theme.serif(28, style: .title1))
                    .foregroundStyle(Theme.ink)
            }
            .padding(.bottom, 14)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if receipts.isEmpty, others.isEmpty {
                        Text("Nothing caught yet. The dock at the bottom of today is the fastest way in.")
                            .font(.system(size: 16))
                            .foregroundStyle(Theme.dim)
                            .padding(.top, 8)
                    }
                    ForEach(receipts, id: \.record.id) { receipt in
                        ReceiptRow(title: receipt.record.title, detail: detail(for: receipt.record, state: receipt.state), dot: dot(for: receipt.state), muted: receipt.state == .gone)
                    }
                    ForEach(others, id: \.path) { note in
                        ReceiptRow(title: note.title, detail: Text(sourceName(note.source) + " · ") + Text("waiting for the desk").foregroundStyle(Theme.muted), dot: Theme.dim, muted: false)
                    }
                }
            }
        }
        .padding(.horizontal, 20)
    }

    private func dot(for state: ReceiptState) -> Color {
        switch state {
        case .filed, .addedToToday, .addedToBacklog: return Theme.good
        default: return Theme.dim
        }
    }

    private func sourceName(_ source: String?) -> String {
        switch source {
        case "google-tasks": return "Google Tasks"
        case "gmail": return "Email"
        case "thock-ios": return "Phone"
        case nil: return "Written at the desk"
        case let other?: return other.capitalized
        }
    }

    private func when(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.dateFormat = Calendar.current.isDateInToday(date) ? "HH:mm" : "EEE"
        return formatter.string(from: date)
    }

    private func when(_ day: VaultDay?) -> String {
        guard let day else { return "" }
        let distance = day.days(until: .today())
        if distance == 0 { return " · today" }
        if distance < 7 { return " · " + String(day.weekdayName.prefix(3)) }
        return " · \(day.formatted("MMM D"))"
    }

    private func detail(for record: CaptureRecord, state: ReceiptState) -> Text {
        let kind = record.kind == .link ? "Link" : "Idea"
        let emphasis = Theme.muted
        switch state {
        case .waiting:
            return Text("\(kind) · \(when(record.madeAt)) · ") + Text("waiting for the desk").foregroundStyle(emphasis)
        case .filed(let destination, let day):
            return Text("Filed · ") + Text(destination).foregroundStyle(emphasis) + Text(when(day))
        case .discarded(let day):
            return Text("Discarded at the desk" + when(day))
        case .addedToToday:
            return Text("\(kind) · \(when(record.madeAt)) · ") + Text("added to Today").foregroundStyle(emphasis)
        case .addedToBacklog:
            return Text("\(kind) · \(when(record.madeAt)) · ") + Text("added to Backlog · Soon").foregroundStyle(emphasis)
        case .gone:
            return Text("\(kind) · \(when(record.madeAt)) · no longer in the inbox")
        }
    }
}

struct ReceiptRow: View {
    var title: String
    var detail: Text
    var dot: Color
    var muted: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top, spacing: 12) {
                Circle().fill(dot).frame(width: 9, height: 9).padding(.top, 7)
                VStack(alignment: .leading, spacing: 3) {
                    Text(title)
                        .font(.system(size: 17))
                        .foregroundStyle(muted ? Theme.dim : Theme.ink)
                        .lineLimit(2)
                    detail
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.dim)
                }
            }
            .padding(.vertical, 11)
            Hairline(color: Theme.ruleSoft)
        }
        .accessibilityElement(children: .combine)
    }
}

/// Ask is not in this release; the tab explains itself and offers nothing
/// else (V33 §11, §18).
struct AskScreen: View {
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SheetHeader(leading: "", title: "", trailing: "Done", onLeading: {}, onTrailing: { dismiss() })
                .padding(.bottom, 4)
            VStack(alignment: .leading, spacing: 4) {
                Text("ASK · READS YOUR VAULT")
                    .font(Theme.label())
                    .tracking(0.9)
                    .foregroundStyle(Theme.dim)
                Text("Thock Agent")
                    .font(Theme.serif(28, style: .title1))
                    .foregroundStyle(Theme.ink)
            }
            .padding(.bottom, 16)
            Text("Asking from the phone isn't here yet. When it arrives, this is where the Thock Agent you talk to at the desk will answer from your own notes, and name the ones it read. For now, a question you write down lands in your inbox and is there when you sit down.")
                .font(.system(size: 17))
                .foregroundStyle(Theme.ink)
                .lineSpacing(4)
                .padding(.vertical, 12)
                .padding(.horizontal, 14)
                .background(Theme.amberSoft)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.amber).frame(width: 2) }
                .clipShape(UnevenRoundedRectangle(bottomTrailingRadius: 10, topTrailingRadius: 10))
            Spacer()
        }
        .padding(.horizontal, 20)
    }
}

/// The only settings the phone has: which desk it is connected to, how it
/// looks, and, in the practice notebook, the pretend desk's controls.
struct YouSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var confirmingDisconnect = false
    @State private var busy = false

    private var status: String {
        let waiting = model.waitingForDesk
        switch model.syncState {
        case .paused: return "Thock Plus has ended. This phone shows what it has and writes nothing new."
        case .disconnected: return "This phone is no longer connected to your desk."
        case .offline:
            return waiting == 0 ? "Can't reach your desk's copy right now. This phone keeps working on what it has."
                : "Can't reach your desk's copy right now. \(waiting) \(waiting == 1 ? "change is" : "changes are") kept here and will be sent."
        default:
            return waiting == 0 ? "Everything written here has reached your desk." : "\(waiting) \(waiting == 1 ? "change is" : "changes are") waiting for the desk."
        }
    }

    /// The facts behind the status sentence, for when notes are not arriving.
    private var details: String {
        let d = model.diagnostics
        var lines: [String] = []
        lines.append("notes here: \(model.store?.paths().count ?? 0) · at the desk's copy: \(d.serverFileCount.map(String.init) ?? "?")")
        lines.append("version \(d.cursor) of \(d.serverLatestVersion.map(String.init) ?? "?") · waiting on the server: \(d.serverPendingWrites.map(String.init) ?? "?")")
        lines.append("address: \(model.store?.meta("backend") ?? "?")")
        if let last = d.lastRound {
            lines.append("last check: " + last.formatted(date: .omitted, time: .standard))
        }
        if let error = d.lastError {
            lines.append("problem: " + error)
        }
        for failure in d.failures.prefix(6) {
            lines.append("could not take: " + failure)
        }
        if d.failures.count > 6 {
            lines.append("and \(d.failures.count - 6) more")
        }
        return lines.joined(separator: "\n")
    }

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 0) {
            SheetHeader(leading: "", title: "", trailing: "Done", onLeading: {}, onTrailing: { dismiss() })
                .padding(.bottom, 4)
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    VStack(alignment: .leading, spacing: 6) {
                        CardLabel(title: "This phone")
                        Text(model.isPractice ? "Practice notebook" : (model.store?.meta("device_name") ?? "Connected"))
                            .font(Theme.serif(24, style: .title2))
                            .foregroundStyle(Theme.ink)
                        Text(status)
                            .font(.system(size: 15))
                            .foregroundStyle(Theme.muted)
                    }

                    if !model.isPractice {
                        VStack(alignment: .leading, spacing: 8) {
                            CardLabel(title: "Details")
                            Text(details)
                                .font(Theme.mono(12))
                                .foregroundStyle(Theme.muted)
                                .textSelection(.enabled)
                            deskButton("Check again") { await model.checkAgain() }
                        }
                    }

                    VStack(alignment: .leading, spacing: 10) {
                        CardLabel(title: "Looks")
                        Picker("Looks", selection: $model.appearance) {
                            ForEach(Appearance.allCases) { Text($0.title).tag($0) }
                        }
                        .pickerStyle(.segmented)
                    }

                    if model.isPractice {
                        VStack(alignment: .leading, spacing: 12) {
                            CardLabel(title: "The practice desk")
                            Text("There is no real desk behind this notebook. These stand in for one, so you can watch a capture travel.")
                                .font(.system(size: 14))
                                .foregroundStyle(Theme.dim)
                            Toggle("The desk is open", isOn: Binding(get: { model.deskAwake }, set: { awake in
                                Task { await run { await model.setDeskAwake(awake) } }
                            }))
                            .toggleStyle(ThockToggleStyle())
                            Text(model.deskAwake ? "What you write reaches the desk right away." : "Closed: what you write waits on this phone until the desk opens.")
                                .font(.system(size: 13))
                                .foregroundStyle(Theme.dim)
                            deskButton("Sort the inbox at the desk") { await model.triageAtTheDesk() }
                            deskButton("Add a line to today from the desk") { await model.planAtTheDesk() }
                            Toggle("Thock Plus has ended", isOn: Binding(get: { model.isReadOnly }, set: { lapsed in
                                Task { await run { await model.setPracticeLapsed(lapsed) } }
                            }))
                            .toggleStyle(ThockToggleStyle())
                        }
                        .font(.system(size: 16))
                        .foregroundStyle(Theme.ink)
                    }

                    VStack(alignment: .leading, spacing: 8) {
                        Hairline()
                        Button(model.isPractice ? "Close the practice notebook" : "Disconnect this phone") {
                            confirmingDisconnect = true
                        }
                        .font(.system(size: 16, weight: .medium))
                        .foregroundStyle(Theme.warn)
                        .padding(.top, 6)
                        Text(model.isPractice ? "The practice notes are removed from this phone." : "Your notes stay at your desk. This phone forgets them.")
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.dim)
                    }
                }
                .padding(.bottom, 24)
            }
            .disabled(busy)
        }
        .padding(.horizontal, 20)
        .confirmationDialog(model.isPractice ? "Close the practice notebook?" : "Disconnect this phone?", isPresented: $confirmingDisconnect, titleVisibility: .visible) {
            Button(model.isPractice ? "Close it" : "Disconnect", role: .destructive) {
                Task { await model.disconnect() }
            }
        } message: {
            Text(model.waitingForDesk > 0 ? "\(model.waitingForDesk) changes haven't reached the desk yet and will be lost." : "You can connect again from your desk at any time.")
        }
    }

    private func run(_ work: () async -> Void) async {
        busy = true
        await work()
        busy = false
    }

    private func deskButton(_ title: String, action: @escaping () async -> Void) -> some View {
        Button {
            Task { await run(action) }
        } label: {
            Text(title)
                .font(.system(size: 16, weight: .medium))
                .foregroundStyle(Theme.amber)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 11)
                .padding(.horizontal, 14)
                .background(Theme.ground, in: RoundedRectangle(cornerRadius: 12))
                .overlay(RoundedRectangle(cornerRadius: 12).stroke(Theme.rule, lineWidth: 1))
        }
        .buttonStyle(.plain)
    }
}

// MARK: Nudges

struct SetTimeSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    var item: PlannerItem
    var day: VaultDay

    @State private var start = Date()
    @State private var end = Date()
    @State private var hasEnd = false

    private func date(minutes: Int) -> Date {
        Calendar.current.date(bySettingHour: min(minutes / 60, 23), minute: minutes % 60, second: 0, of: Date()) ?? Date()
    }

    private func minutes(_ date: Date) -> Int {
        let parts = Calendar.current.dateComponents([.hour, .minute], from: date)
        return (parts.hour ?? 0) * 60 + (parts.minute ?? 0)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            SheetHeader(leading: "No time", title: "Set a time", trailing: "Done") {
                model.perform { try $0.setTime(item, startMinutes: nil, endMinutes: nil, day: day) }
                dismiss()
            } onTrailing: {
                let from = minutes(start)
                let until = minutes(end)
                model.perform { try $0.setTime(item, startMinutes: from, endMinutes: hasEnd && until > from ? until : nil, day: day) }
                dismiss()
            }
            Text(Inline.plainText(item.label))
                .font(.system(size: 17))
                .foregroundStyle(Theme.muted)
                .lineLimit(2)
            DatePicker("Starts", selection: $start, displayedComponents: .hourAndMinute)
            Toggle("Has an end", isOn: $hasEnd)
                .toggleStyle(ThockToggleStyle())
            if hasEnd {
                DatePicker("Ends", selection: $end, displayedComponents: .hourAndMinute)
            }
            Spacer(minLength: 0)
        }
        .font(.system(size: 17))
        .foregroundStyle(Theme.ink)
        .padding(.horizontal, 20)
        .environment(\.locale, Locale(identifier: "en_GB"))
        .onAppear {
            if let time = item.time {
                start = date(minutes: time.startMinutes)
                hasEnd = time.endMinutes != nil
                end = date(minutes: time.endMinutes ?? time.startMinutes + 30)
            } else {
                let now = minutes(Date())
                let rounded = min(((now + 29) / 30) * 30, 23 * 60 + 30)
                start = date(minutes: rounded)
                end = date(minutes: min(rounded + 30, 23 * 60 + 59))
            }
        }
        .onChange(of: start) { _, newStart in
            if minutes(end) <= minutes(newStart) {
                end = date(minutes: min(minutes(newStart) + 30, 23 * 60 + 59))
            }
        }
    }
}

struct EditLineSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    var item: PlannerItem
    var day: VaultDay

    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            SheetHeader(leading: "Cancel", title: "Edit this line", trailing: "Done", trailingEnabled: !text.trimmingCharacters(in: .whitespaces).isEmpty) {
                dismiss()
            } onTrailing: {
                model.perform { try $0.editText(item, text: text, day: day) }
                dismiss()
            }
            TextField("The line", text: $text, axis: .vertical)
                .font(.system(size: 19))
                .foregroundStyle(Theme.ink)
                .lineLimit(1...4)
                .focused($focused)
            Text("Its checkbox and time stay as they are.")
                .font(.system(size: 13))
                .foregroundStyle(Theme.dim)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 20)
        .onAppear {
            text = TaskLineParts(item.raw)?.text ?? item.label
            focused = true
        }
    }
}
