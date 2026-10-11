import PhotosUI
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

/// The feed behind the Inbox row: what this phone sent to the inbox and what
/// became of it (V33 §6.3), plus whatever else waits there. Captures sent to
/// Today or the backlog never pass through the inbox, so they are not listed.
/// A note still waiting can be opened and edited, or swiped: right for
/// Today or the backlog, left to archive (V40 §4).
struct ReceiptsScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var editing: InboxEdit?

    private static let rowInsets = EdgeInsets(top: 0, leading: 20, bottom: 0, trailing: 20)

    var body: some View {
        let _ = model.revision
        let receipts = (model.store?.receipts() ?? []).filter { receipt in
            receipt.record.inboxPath.map { !model.isBeingTriaged($0) } ?? false
        }
        let mine = Set(receipts.compactMap(\.record.inboxPath))
        let others = (model.store?.waitingInboxNotes() ?? []).filter { !mine.contains($0.path) && !model.isBeingTriaged($0.path) }
        VStack(alignment: .leading, spacing: 0) {
            SheetHeader(leading: "", title: "", trailing: "Done", onLeading: {}, onTrailing: { dismiss() })
                .padding(.bottom, 4)
            VStack(alignment: .leading, spacing: 4) {
                Text("INBOX")
                    .font(Theme.label())
                    .tracking(0.9)
                    .foregroundStyle(Theme.dim)
                Text("Waiting for the desk")
                    .font(Theme.serif(28, style: .title1))
                    .foregroundStyle(Theme.ink)
            }
            .padding(.horizontal, 20)
            .padding(.bottom, 14)
            List {
                if receipts.isEmpty, others.isEmpty {
                    Text("Nothing in the inbox. The field at the bottom of today is the fastest way in.")
                        .font(.system(size: 16))
                        .foregroundStyle(Theme.dim)
                        .padding(.top, 8)
                        .listRowBackground(Color.clear)
                        .listRowSeparator(.hidden)
                        .listRowInsets(Self.rowInsets)
                }
                ForEach(receipts, id: \.record.id) { receipt in
                    let waiting = receipt.state == .waiting ? receipt.record.inboxPath : nil
                    ReceiptRow(title: receipt.record.title, detail: detail(for: receipt.record, state: receipt.state), dot: dot(for: receipt.state), muted: receipt.state == .gone,
                               onEdit: waiting.flatMap(edit(_:)))
                        .modifier(InboxGestures(path: waiting, enabled: !model.isReadOnly, onEdit: waiting.flatMap(edit(_:))))
                        .listRowBackground(Color.clear)
                        .listRowSeparator(.hidden)
                        .listRowInsets(Self.rowInsets)
                }
                ForEach(others, id: \.path) { note in
                    ReceiptRow(title: note.title, detail: Text(sourceName(note.source) + " · ") + Text("waiting for the desk").foregroundStyle(Theme.muted), dot: Theme.dim, muted: false,
                               onEdit: edit(note.path))
                        .modifier(InboxGestures(path: note.path, enabled: !model.isReadOnly, onEdit: edit(note.path)))
                        .listRowBackground(Color.clear)
                        .listRowSeparator(.hidden)
                        .listRowInsets(Self.rowInsets)
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .environment(\.defaultMinListRowHeight, 1)
        }
        .sheet(item: $editing) { edit in
            InboxEditSheet(edit: edit)
                .presentationBackground(Theme.surface)
                .presentationCornerRadius(26)
                .presentationDragIndicator(.hidden)
                .preferredColorScheme(model.appearance.scheme)
                .tint(Theme.amber)
        }
    }

    /// Opens the editor on a waiting note, when it has a heading to anchor
    /// the edit and the phone may still write.
    private func edit(_ path: String) -> (() -> Void)? {
        guard !model.isReadOnly, let text = model.session?.inboxEditorText(path) else { return nil }
        return { editing = InboxEdit(path: path, text: text) }
    }

    private func dot(for state: ReceiptState) -> Color {
        switch state {
        case .filed, .addedToToday, .addedToBacklog, .archived: return Theme.good
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
        case .archived(let day):
            return Text("Archived" + when(day))
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
    var onEdit: (() -> Void)?

    var body: some View {
        let row = VStack(alignment: .leading, spacing: 0) {
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
                .frame(maxWidth: .infinity, alignment: .leading)
                if onEdit != nil {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.dim)
                        .padding(.top, 6)
                }
            }
            .padding(.vertical, 11)
            Hairline(color: Theme.ruleSoft)
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)

        if let onEdit {
            Button(action: onEdit) { row }
                .buttonStyle(.plain)
                .accessibilityHint("Opens it to edit")
        } else {
            row
        }
    }
}

/// The three decisions a waiting row takes without the ritual (V40 §4):
/// swipe right for Today (a full swipe) or the backlog, left to archive, and
/// the same three in the long-press menu beside Edit, so every gesture has a
/// path VoiceOver can take. A row with nothing waiting gets none.
private struct InboxGestures: ViewModifier {
    @Environment(AppModel.self) private var model
    var path: String?
    var enabled: Bool
    var onEdit: (() -> Void)?

    func body(content: Content) -> some View {
        if let path, enabled {
            content
                .swipeActions(edge: .leading, allowsFullSwipe: true) {
                    Button("Today") { model.inboxGesture(.today, path: path) }
                        .tint(Theme.amber)
                    Button("Backlog") { model.inboxGesture(.backlog, path: path) }
                        .tint(Theme.good)
                }
                .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                    Button("Archive") { model.inboxGesture(.archive, path: path) }
                        .tint(Theme.dim)
                }
                .contextMenu {
                    Button("Add to today") { model.inboxGesture(.today, path: path) }
                    Button("Add to the backlog") { model.inboxGesture(.backlog, path: path) }
                    Button("Archive") { model.inboxGesture(.archive, path: path) }
                    if let onEdit {
                        Button("Edit", action: onEdit)
                    }
                }
        } else {
            content
        }
    }
}

struct InboxEdit: Identifiable {
    var id: String { path }
    var path: String
    var text: String
}

/// A waiting inbox note in the capture editor. Done saves, and so does
/// swiping it away, as with a new capture; Cancel leaves the note as it was.
struct InboxEditSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    var edit: InboxEdit

    @State private var blocks: [Block] = []
    @State private var images: [ImageAttachment] = []
    @State private var handle = EditorHandle()
    @State private var finished = false

    /// Nothing the person can see is left; what the editor carries unseen
    /// (a table, a code block) is not a capture on its own.
    private var isEmpty: Bool {
        blocks.allSatisfy { EditorKind(displaying: $0.kind) == nil }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            SheetHeader(leading: "Cancel", title: "Edit capture", trailing: "Done", trailingEnabled: !isEmpty) {
                finished = true
                dismiss()
            } onTrailing: {
                save()
                dismiss()
            }
            RichTextEditor(initial: Blocks.parse(edit.text), handle: handle, placeholder: "What's on your mind?", fontSize: 19) { query in
                model.session?.noteTitles(matching: query) ?? []
            } onChange: { blocks = $0 }
            .frame(maxHeight: .infinity)
            AttachmentStrip(images: $images)
            Text("The first line is its title. It stays in your inbox for triage.")
                .font(.system(size: 13))
                .foregroundStyle(Theme.dim)
                .padding(.bottom, 10)
        }
        .padding(.horizontal, 20)
        .onAppear { blocks = Blocks.parse(edit.text) }
        .onDisappear(perform: save)
    }

    private func save() {
        guard !finished else { return }
        finished = true
        guard !isEmpty, images.isEmpty || !blocks.isEmpty else { return }
        guard !images.isEmpty || EditorDocument(blocks: blocks).lines().joined(separator: "\n") != edit.text else { return }
        var changed = false
        if model.perform({ changed = try $0.editInbox(path: edit.path, blocks: blocks, images: images) }), changed {
            model.show(images.isEmpty ? "Capture updated" : "Capture updated, \(images.count == 1 ? "picture" : "pictures") attached")
        }
    }
}

/// The only settings the phone has: how it is doing with the desk, a way to
/// reach a person when it is not, how it looks, and the way out. The practice
/// notebook adds the pretend desk's controls.
struct YouSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var confirmingDisconnect = false
    @State private var reporting = false
    @State private var busy = false

    private var status: String {
        let waiting = model.waitingForDesk
        switch model.syncState {
        case .paused: return "Thock Plus has ended. This phone shows what it has and writes nothing new."
        case .disconnected: return "This phone is no longer connected to your desk."
        case .working: return "Sending what's new and fetching what changed."
        case .offline:
            return waiting == 0 ? "This phone keeps working on what it has."
                : "\(waiting) \(waiting == 1 ? "change is" : "changes are") kept here and will be sent."
        default:
            return waiting == 0 ? "Everything written here has reached your desk." : "They reach your notes when your desk is open."
        }
    }

    private var lastChecked: String {
        guard let last = model.diagnostics.lastRound else { return "Not checked yet" }
        let formatter = DateFormatter()
        formatter.dateFormat = Calendar.current.isDateInToday(last) ? "HH:mm" : "EEE HH:mm"
        return "Last checked " + formatter.string(from: last)
    }

    private var disconnectWarning: String {
        if model.isPractice {
            return "The practice notes are removed from this phone."
        }
        let lost = model.waitingForDesk > 0 ? "\(model.waitingForDesk) \(model.waitingForDesk == 1 ? "change hasn't" : "changes haven't") reached the desk yet and will be lost. " : ""
        return lost + "Your desk keeps listing this phone until you disconnect it there as well. To use this phone again, connect it from your desk."
    }

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 0) {
            SheetHeader(leading: "", title: "", trailing: "Done", onLeading: {}, onTrailing: { dismiss() })
                .padding(.bottom, 4)
            ScrollView {
                VStack(alignment: .leading, spacing: 26) {
                    VStack(alignment: .leading, spacing: 10) {
                        CardLabel(title: "This phone")
                        Text(model.isPractice ? "Practice notebook" : (model.store?.meta("device_name") ?? "Connected"))
                            .font(Theme.serif(24, style: .title2))
                            .foregroundStyle(Theme.ink)
                        ConnectionStatus(state: model.syncState, waiting: model.waitingForDesk, sentence: status)
                            .padding(.top, 2)
                        HStack(alignment: .firstTextBaseline) {
                            Text(lastChecked)
                                .font(.system(size: 13))
                                .foregroundStyle(Theme.dim)
                            Spacer(minLength: 12)
                            Button("Check again") {
                                Task { await run { await model.checkAgain() } }
                            }
                            .font(.system(size: 13, weight: .medium))
                            .foregroundStyle(Theme.amber)
                            .disabled(model.syncState == .working || model.syncState == .notConnected)
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

                    if !model.isPractice, model.store?.isConnected == true {
                        VStack(alignment: .leading, spacing: 8) {
                            CardLabel(title: "Help")
                            deskButton("Report a problem") { reporting = true }
                            Text("Tell us what went wrong, with a screenshot if it helps. A few details about this phone's connection go with it, never your notes.")
                                .font(.system(size: 13))
                                .foregroundStyle(Theme.dim)
                        }
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

                    Text(AppModel.versionLabel)
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.dim)
                        .frame(maxWidth: .infinity)
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
            Text(disconnectWarning)
        }
        .sheet(isPresented: $reporting) {
            ReportProblemSheet()
                .presentationBackground(Theme.surface)
                .presentationCornerRadius(26)
                .presentationDragIndicator(.hidden)
                .preferredColorScheme(model.appearance.scheme)
                .tint(Theme.amber)
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

/// The connection at a glance: a glyph in the state's colour, a headline,
/// and the sentence that says what it means. Same dot language as the Inbox
/// receipts.
struct ConnectionStatus: View {
    var state: SyncState
    var waiting: Int
    var sentence: String

    private var glyph: (name: String, tone: Color, headline: String) {
        switch state {
        case .notConnected: return ("circle.dashed", Theme.dim, "Not connected")
        case .working: return ("arrow.triangle.2.circlepath.circle", Theme.amber, "Checking with your desk")
        case .upToDate where waiting > 0: return ("circle.lefthalf.filled", Theme.amber, "\(waiting) waiting for the desk")
        case .upToDate: return ("checkmark.circle.fill", Theme.good, "Up to date")
        case .offline: return ("exclamationmark.circle", Theme.warn, "Can't reach your desk")
        case .paused: return ("pause.circle", Theme.dim, "Thock Plus has ended")
        case .disconnected: return ("xmark.circle", Theme.warn, "Disconnected by your desk")
        }
    }

    var body: some View {
        let glyph = glyph
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: glyph.name)
                .font(.system(size: 20, weight: .medium))
                .foregroundStyle(glyph.tone)
                .frame(width: 24)
                .padding(.top, 1)
            VStack(alignment: .leading, spacing: 3) {
                Text(glyph.headline)
                    .font(.system(size: 17, weight: .medium))
                    .foregroundStyle(Theme.ink)
                Text(sentence)
                    .font(.system(size: 14))
                    .foregroundStyle(Theme.muted)
            }
        }
        .animation(.snappy(duration: 0.2), value: state)
        .accessibilityElement(children: .combine)
    }
}

/// A picture on its way into a report: shown as a thumbnail, sent as JPEG.
struct Screenshot: Identifiable {
    let id = UUID()
    var image: UIImage
    var data: Data

    /// Keeps a picture small enough to send: at most 1600 pixels on its
    /// longest side, as JPEG. Nil for data that isn't a picture.
    static func prepare(_ raw: Data) -> Screenshot? {
        guard let image = UIImage(data: raw), image.size.width > 0, image.size.height > 0 else { return nil }
        let pixels = CGSize(width: image.size.width * image.scale, height: image.size.height * image.scale)
        let factor = min(1, 1600 / max(pixels.width, pixels.height))
        let size = CGSize(width: (pixels.width * factor).rounded(), height: (pixels.height * factor).rounded())
        let format = UIGraphicsImageRendererFormat.default()
        format.scale = 1
        let scaled = UIGraphicsImageRenderer(size: size, format: format).image { _ in
            image.draw(in: CGRect(origin: .zero, size: size))
        }
        guard let jpeg = scaled.jpegData(compressionQuality: 0.8) else { return nil }
        return Screenshot(image: scaled, data: jpeg)
    }
}

/// Report a problem (V41): what happened in the person's words, screenshots
/// if they help, and the facts that go with it, shown before anything is
/// sent. Send files it for us to read; nothing leaves the phone otherwise.
struct ReportProblemSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var picks: [PhotosPickerItem] = []
    @State private var screenshots: [Screenshot] = []
    @State private var sending = false
    @State private var showingDetails = false
    @State private var failure: String?
    @FocusState private var focused: Bool

    private var description: String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            SheetHeader(leading: "Cancel", title: "Report a problem", trailing: sending ? "Sending…" : "Send", trailingEnabled: !sending && !description.isEmpty) {
                dismiss()
            } onTrailing: {
                send()
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    TextField("What happened? What did you expect instead?", text: $text, axis: .vertical)
                        .font(.system(size: 17))
                        .foregroundStyle(Theme.ink)
                        .lineLimit(4...12)
                        .focused($focused)

                    VStack(alignment: .leading, spacing: 10) {
                        CardLabel(title: "Screenshots", note: screenshots.isEmpty ? nil : "\(screenshots.count) of \(IssueReport.maxScreenshots)")
                        HStack(spacing: 10) {
                            ForEach(screenshots) { shot in
                                thumbnail(shot)
                            }
                            if screenshots.count < IssueReport.maxScreenshots {
                                PhotosPicker(selection: $picks, maxSelectionCount: IssueReport.maxScreenshots - screenshots.count, matching: .images) {
                                    Image(systemName: "plus")
                                        .font(.system(size: 20, weight: .medium))
                                        .foregroundStyle(Theme.amber)
                                        .frame(width: 72, height: 72)
                                        .background(Theme.ground, in: RoundedRectangle(cornerRadius: 12))
                                        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Theme.rule, style: StrokeStyle(lineWidth: 1, dash: [5, 4])))
                                }
                                .accessibilityLabel("Add a screenshot")
                            }
                        }
                        Text("Optional. A picture of what went wrong says a lot.")
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.dim)
                    }

                    VStack(alignment: .leading, spacing: 8) {
                        CardLabel(title: "Sent with it")
                        Text("A few details about this phone's connection, never your notes.")
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.dim)
                        Button(showingDetails ? "Hide the details" : "Show the details") {
                            showingDetails.toggle()
                        }
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(Theme.amber)
                        if showingDetails {
                            Text(model.issueReport().details)
                                .font(Theme.mono(12))
                                .foregroundStyle(Theme.muted)
                                .textSelection(.enabled)
                        }
                    }
                }
                .padding(.bottom, 24)
            }
            .disabled(sending)
        }
        .padding(.horizontal, 20)
        .onAppear { focused = true }
        .onChange(of: picks) { _, items in
            guard !items.isEmpty else { return }
            Task { await add(items) }
        }
        .alert("Couldn't send the report", isPresented: Binding(get: { failure != nil }, set: { if !$0 { failure = nil } })) {
            Button("OK") {}
        } message: {
            Text(failure ?? "")
        }
    }

    private func thumbnail(_ shot: Screenshot) -> some View {
        Image(uiImage: shot.image)
            .resizable()
            .scaledToFill()
            .frame(width: 72, height: 72)
            .clipShape(RoundedRectangle(cornerRadius: 12))
            .overlay(alignment: .topTrailing) {
                Button {
                    screenshots.removeAll { $0.id == shot.id }
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 18))
                        .foregroundStyle(Theme.ink, Theme.surface)
                        .padding(4)
                }
                .accessibilityLabel("Remove this screenshot")
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Screenshot")
    }

    private func add(_ items: [PhotosPickerItem]) async {
        for item in items where screenshots.count < IssueReport.maxScreenshots {
            guard let raw = try? await item.loadTransferable(type: Data.self), let shot = Screenshot.prepare(raw) else {
                failure = "That picture couldn't be read."
                continue
            }
            screenshots.append(shot)
        }
        picks = []
    }

    private func send() {
        sending = true
        Task {
            do {
                _ = try await model.sendReport(description: description, screenshots: screenshots.map(\.data))
                UINotificationFeedbackGenerator().notificationOccurred(.success)
                model.show("Sent. Thank you.")
                model.sheet = nil
            } catch {
                failure = AppModel.sentence(for: error)
            }
            sending = false
        }
    }
}

// MARK: Nudges

struct SetTimeSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    var item: PlannerItem
    var note: NoteID

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
                model.perform { try $0.setTime(item, startMinutes: nil, endMinutes: nil, note: note) }
                dismiss()
            } onTrailing: {
                let from = minutes(start)
                let until = minutes(end)
                model.perform { try $0.setTime(item, startMinutes: from, endMinutes: hasEnd && until > from ? until : nil, note: note) }
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
    var note: NoteID

    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            SheetHeader(leading: "Cancel", title: "Edit this line", trailing: "Done", trailingEnabled: !text.trimmingCharacters(in: .whitespaces).isEmpty) {
                dismiss()
            } onTrailing: {
                model.perform { try $0.editText(item, text: text, note: note) }
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
