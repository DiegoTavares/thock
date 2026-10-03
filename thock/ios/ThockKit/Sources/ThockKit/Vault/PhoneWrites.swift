import CryptoKit
import Foundation

public enum NoteKind: String, Codable, Sendable {
    case daily
    case weekly
}

/// The template a write may create its note from. Device-local: the write
/// document only says `create_from_template`, the phone remembers which day.
public struct SeedInfo: Equatable, Codable, Sendable {
    public var kind: NoteKind
    public var day: VaultDay
    public var time: String
}

public struct PlannedWrite: Equatable, Sendable {
    public var document: WriteDocument
    public var seed: SeedInfo?
}

public enum CaptureDestination: String, Codable, CaseIterable, Sendable {
    case inbox
    case today
    case backlog
}

public enum CaptureKind: String, Codable, Sendable {
    case idea
    case link
}

/// The phone's own record of a capture (V33 §14), so receipts can say what
/// became of it even when it left no inbox note.
public struct CaptureRecord: Equatable, Identifiable, Sendable {
    public var id: Int64 = 0
    public var digest: String
    public var title: String
    public var kind: CaptureKind
    public var destination: CaptureDestination
    public var madeAt: Date
    public var inboxPath: String?
}

public struct TaskLineParts: Equatable, Sendable {
    /// Indent, bullet, checkbox and the space after it.
    public var head: String
    public var time: String?
    public var text: String
    /// Trailing comments with the space before them.
    public var tail: String

    public init?(_ raw: String) {
        let indent = raw.prefix { $0 == " " || $0 == "\t" }
        var rest = raw.dropFirst(indent.count)
        guard let marker = rest.first, "-*+".contains(marker) else { return nil }
        rest = rest.dropFirst()
        let gap = rest.prefix { $0 == " " || $0 == "\t" }
        guard !gap.isEmpty else { return nil }
        rest = rest.dropFirst(gap.count)
        guard rest.hasPrefix("[ ]") || rest.hasPrefix("[x]") || rest.hasPrefix("[X]") else { return nil }
        let box = rest.prefix(3)
        rest = rest.dropFirst(3)
        let space = rest.prefix { $0 == " " || $0 == "\t" }
        rest = rest.dropFirst(space.count)
        head = String(indent) + String(marker) + String(gap) + String(box) + (space.isEmpty ? " " : String(space))
        let body = NoteView.strippingTrailingComments(String(rest))
        tail = String(rest.dropFirst(body.count))
        if let time = TimePrefix.parse(body) {
            self.time = time.raw
            text = String(time.rest)
        } else {
            text = body
        }
    }

    public var done: Bool {
        head.contains("[x]") || head.contains("[X]")
    }

    public func line(done: Bool? = nil) -> String {
        var head = head
        if let done {
            for box in ["[ ]", "[x]", "[X]"] {
                if let range = head.range(of: box) {
                    head.replaceSubrange(range, with: done ? "[x]" : "[ ]")
                    break
                }
            }
        }
        return head + (time.map { text.isEmpty ? $0 : $0 + " " } ?? "") + text + tail
    }
}

/// Builds the writes the phone is allowed to make (V33 §14) from what the
/// person did. Pure: the store applies and queues what these return.
public struct PhoneWrites: Sendable {
    public var config: VaultConfig
    public var deviceID: String
    public var now: Date
    public var calendar: Calendar

    public init(config: VaultConfig, deviceID: String, now: Date = Date(), calendar: Calendar = .current) {
        self.config = config
        self.deviceID = deviceID
        self.now = now
        self.calendar = calendar
    }

    public var today: VaultDay { VaultDay(now, calendar: calendar) }

    public var clock: String {
        let parts = calendar.dateComponents([.hour, .minute], from: now)
        return String(format: "%02d:%02d", parts.hour ?? 0, parts.minute ?? 0)
    }

    var madeAt: String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.string(from: now)
    }

    /// Local time with its offset, as the desk stamps `captured:`.
    var capturedAt: String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        formatter.timeZone = calendar.timeZone
        return formatter.string(from: now)
    }

    func document(_ kind: WriteKind, path: String) -> WriteDocument {
        WriteDocument(clientID: UUID().uuidString.lowercased(), kind: kind, path: path, madeAt: madeAt, deviceID: deviceID)
    }

    public func dailySeed(_ day: VaultDay) -> SeedInfo {
        SeedInfo(kind: .daily, day: day, time: clock)
    }

    /// The first 12 hex of `sha256(device ‖ 0 ‖ "thock-ios" ‖ 0 ‖ instant)`:
    /// the desk's capture digest (V13 §6) with the device as the account and
    /// the capture instant as the item id.
    public func captureDigest(salt: String = "") -> String {
        var data = Data(deviceID.utf8)
        data.append(0)
        data.append(Data("thock-ios".utf8))
        data.append(0)
        data.append(Data((String(format: "%.6f", now.timeIntervalSince1970) + salt).utf8))
        return String(SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined().prefix(12))
    }

    // MARK: Capture

    public struct InboxFields {
        public var title: String
        public var body: String
        public var url: String?
        public var kind: String?

        public init(title: String, body: String, url: String? = nil, kind: String? = nil) {
            self.title = title
            self.body = body
            self.url = url
            self.kind = kind
        }
    }

    /// One inbox note in the V13 §6 format, `source: thock-ios`.
    public func inboxNote(_ fields: InboxFields, captureKind: CaptureKind, taken: (String) -> Bool) -> (PlannedWrite, CaptureRecord) {
        let digest = captureDigest()
        let title = Slug.sanitizedTitle(fields.title)
        let parts = calendar.dateComponents([.hour, .minute], from: now)
        let stamp = today.iso + "-" + String(format: "%02d%02d", parts.hour ?? 0, parts.minute ?? 0)
        let base = "\(config.inboxDir)/\(stamp)-\(Slug.make(title))"
        let path = [base, "\(base)-\(digest.prefix(4))", "\(base)-\(digest)"].first { !taken($0 + ".md") } ?? "\(base)-\(digest)"

        var note = "---\n"
        func field(_ key: String, _ value: String) {
            note += (key + ":").padding(toLength: 9, withPad: " ", startingAt: 0) + " " + value + "\n"
        }
        field("source", "thock-ios")
        field("capture", digest)
        field("captured", capturedAt)
        field("title", title)
        if let url = fields.url {
            field("url", url.split(whereSeparator: \.isWhitespace).joined(separator: " "))
        }
        if let kind = fields.kind {
            field("kind", kind)
        }
        note += "---\n\n# \(title)\n"
        let body = fields.body.trimmingCharacters(in: .whitespacesAndNewlines)
        if !body.isEmpty {
            note += "\n" + body + "\n"
        }

        var write = document(.create, path: path + ".md")
        write.content = note
        let record = CaptureRecord(digest: digest, title: title, kind: captureKind, destination: .inbox, madeAt: now, inboxPath: path + ".md")
        return (PlannedWrite(document: write), record)
    }

    /// A capture, routed by its destination chip (V33 §6.2). `blocks` is what
    /// the editor produced; `todayNote` is today's note as the phone has it.
    public func capture(blocks: [Block], destination: CaptureDestination, todayNote: String?, template: String?, taken: (String) -> Bool) -> (writes: [PlannedWrite], record: CaptureRecord)? {
        let content = blocks.filter { $0.kind != .blank }
        guard let first = content.first, !content.allSatisfy({ $0.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && $0.kind != .rule }) else { return nil }
        let firstLines = first.text.components(separatedBy: "\n")
        let title = Inline.plainText(firstLines[0])
        let single = content.count == 1 && firstLines.count == 1

        switch destination {
        case .inbox:
            let body = Self.inboxBody(content).joined(separator: "\n")
            let (write, record) = inboxNote(InboxFields(title: title, body: body), captureKind: .idea, taken: taken)
            return ([write], record)

        case .today:
            let path = config.dailyPath(today)
            let record = CaptureRecord(digest: captureDigest(), title: title, kind: .idea, destination: .today, madeAt: now, inboxPath: nil)
            let view = NoteView(text: todayNote ?? template.map { Template.expand($0, day: today, time: clock, title: today.formatted(config.daily.filename)) } ?? "", config: config)
            if single {
                var write = document(.append, path: path)
                write.heading = view.planner.heading
                write.lines = ["- [ ] " + first.text]
                write.placement = .beforeChildren
                write.createFromTemplate = true
                return ([PlannedWrite(document: write, seed: dailySeed(today))], record)
            }
            var write = document(.append, path: path)
            write.heading = proseHeading(in: view)
            write.lines = EditorDocument(blocks: content.map { var block = $0; block.touched = true; return block }).lines()
            write.blankLineBefore = true
            write.createFromTemplate = true
            return ([PlannedWrite(document: write, seed: dailySeed(today))], record)

        case .backlog:
            var write = document(.append, path: config.backlogFile)
            write.heading = HeadingRef(text: config.soonHeading, level: 2)
            var lines = ["- [ ] " + firstLines[0]]
            // Extra lines stay with their task as its indented continuation.
            for line in firstLines.dropFirst() {
                lines.append("  " + line)
            }
            for block in content.dropFirst() {
                var block = block
                block.touched = true
                lines += block.markdownLines().map { "  " + $0 }
            }
            write.lines = lines
            write.placement = .beforeChildren
            let record = CaptureRecord(digest: captureDigest(), title: title, kind: .idea, destination: .backlog, madeAt: now, inboxPath: nil)
            return ([PlannedWrite(document: write)], record)
        }
    }

    /// `## Personal`, or the last of the user's sections when the note has
    /// no such heading, so prose never lands under the agent's voice.
    func proseHeading(in view: NoteView) -> HeadingRef {
        let userCards = view.cards.filter { $0.kind != .agent && $0.kind != .preamble }
        for name in config.personalHeadings {
            let key = SyncCore.headingKey(name)
            if let card = userCards.first(where: { SyncCore.headingKey($0.heading?.text ?? "") == key }), let heading = card.heading {
                return heading
            }
        }
        if let last = userCards.last(where: { $0.kind == .prose })?.heading ?? userCards.last?.heading {
            return last
        }
        return HeadingRef(text: config.personalHeadings.first ?? "Personal", level: 2)
    }

    /// What follows an inbox note's title: everything the editor produced
    /// after the first line, which is the title.
    static func inboxBody(_ content: [Block]) -> [String] {
        guard let first = content.first else { return [] }
        let firstLines = first.text.components(separatedBy: "\n")
        var rest = Array(content.dropFirst())
        if firstLines.count > 1 {
            var remainder = first
            remainder.text = firstLines.dropFirst().joined(separator: "\n")
            rest.insert(remainder, at: 0)
        }
        return EditorDocument(blocks: rest.map { var block = $0; block.touched = true; return block }).lines()
    }

    // MARK: Inbox edits

    /// A waiting inbox note as the capture editor opens it: the title line,
    /// then the body. `nil` when the note has no level-1 heading, which is
    /// what an edit is anchored on.
    public static func inboxEditorText(_ note: String) -> String? {
        let file = TextFile(note)
        guard let heading = file.headings().first(where: { $0.level == 1 }) else { return nil }
        let body = file.lines[file.section(of: heading).body].map(\.text)
        let trimmed = body.drop { $0.trimmingCharacters(in: .whitespaces).isEmpty }
        return ([heading.text] + (trimmed.isEmpty ? [] : [""] + trimmed)).joined(separator: "\n")
    }

    /// Rewrites a waiting inbox note's title and body, leaving its front
    /// matter as captured (the contract has no write that reaches it). The
    /// body is guarded by its hash, so an edit made at the desk meanwhile is
    /// kept beside this one rather than lost. `blocks` are the editor's, so
    /// a block the person left alone is copied through as it was.
    public func inboxEdit(path: String, note: String, blocks: [Block]) -> (writes: [PlannedWrite], title: String)? {
        let file = TextFile(note)
        let headings = file.headings()
        guard let heading = headings.first(where: { $0.level == 1 }) else { return nil }
        let content = blocks.filter { $0.kind != .blank }
        guard let titleIndex = blocks.firstIndex(where: { EditorKind(displaying: $0.kind) != nil }),
              !content.allSatisfy({ $0.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && $0.kind != .rule })
        else { return nil }
        let first = blocks[titleIndex]
        let title = Slug.sanitizedTitle(Inline.plainText(first.text.components(separatedBy: "\n")[0]))

        var writes: [PlannedWrite] = []
        let section = file.section(of: heading)
        let oldBody = file.lines[section.body].map(\.text)
        var rest = blocks
        rest.remove(at: titleIndex)
        let firstLines = first.text.components(separatedBy: "\n")
        if firstLines.count > 1 {
            rest.insert(Block(id: 0, kind: first.kind, text: firstLines.dropFirst().joined(separator: "\n"), touched: true), at: titleIndex)
        }
        // Blank lines are kept only as they were read; the document puts
        // its own between new blocks.
        rest.removeAll { $0.kind == .blank && $0.source.isEmpty }
        var newBody = Array(EditorDocument(blocks: rest).lines().drop { $0.trimmingCharacters(in: .whitespaces).isEmpty })
        while newBody.last?.trimmingCharacters(in: .whitespaces).isEmpty == true {
            newBody.removeLast()
        }
        if !newBody.isEmpty {
            newBody.insert("", at: 0)
        }
        if newBody != oldBody {
            var write = document(.replaceSection, path: path)
            write.heading = NoteView.reference(heading, in: headings)
            write.baseHash = SyncCore.sectionHash(lines: oldBody)
            write.lines = newBody
            writes.append(PlannedWrite(document: write))
        }
        // The heading is renamed last: the section write above names it by
        // its old text.
        if title != Slug.sanitizedTitle(Inline.plainText(heading.text)) {
            let line = file.lines[heading.index].text
            let hash = SyncCore.lineHash(line)
            let mask = file.contentMask()
            let twins = file.wholeFile().body.filter { mask[$0] && SyncCore.lineHash(file.lines[$0].text) == hash }
            var write = document(.replaceLine, path: path)
            write.lineHash = hash
            write.ordinal = twins.firstIndex(of: heading.index) ?? 0
            write.newLine = "# " + title
            writes.append(PlannedWrite(document: write))
        }
        return (writes, title)
    }

    // MARK: Journal

    /// A new timestamped entry at the end of today's Journal (V33 §9).
    public func journalAppend(blocks: [Block], journal: Journal) -> PlannedWrite? {
        let content = blocks.filter { $0.kind != .blank }.map { block -> Block in
            var block = block
            block.touched = true
            return block
        }
        var lines = EditorDocument(blocks: content).lines()
        guard let first = lines.first, !lines.joined().trimmingCharacters(in: .whitespaces).isEmpty else { return nil }
        lines[0] = "**\(clock)** · " + first
        var write = document(.append, path: config.dailyPath(today))
        write.heading = journal.heading
        write.lines = lines
        write.blankLineBefore = true
        write.createFromTemplate = true
        return PlannedWrite(document: write, seed: dailySeed(today))
    }

    /// Replaces one earlier paragraph. A one-line paragraph is a single-line
    /// replacement; a wrapped one needs the section's lines rewritten around
    /// it, guarded by the section's hash.
    public func journalReplace(entry: JournalEntry, newText: String, day: VaultDay, note: String, journal: Journal) -> PlannedWrite? {
        let path = config.dailyPath(day)
        let prefix = entry.time.map { "**\($0)** · " } ?? ""
        let newLines = (prefix + newText).components(separatedBy: "\n")
        if entry.source.count == 1, newLines.count == 1 {
            guard !SyncCore.lineIdentity(entry.source[0]).isEmpty else { return nil }
            var write = document(.replaceLine, path: path)
            write.heading = journal.heading
            write.lineHash = SyncCore.lineHash(entry.source[0])
            write.ordinal = ordinal(of: entry.line, hash: SyncCore.lineHash(entry.source[0]), in: note, heading: journal.heading)
            write.newLine = newLines[0]
            return PlannedWrite(document: write)
        }
        let file = TextFile(note)
        guard let heading = file.resolve(journal.heading) else { return nil }
        let section = file.section(of: heading)
        var body = file.lines[section.body].map(\.text)
        let start = entry.line - section.start
        guard start >= 0, start + entry.source.count <= body.count else { return nil }
        var write = document(.replaceSection, path: path)
        write.heading = journal.heading
        write.baseHash = SyncCore.sectionHash(lines: body)
        body.replaceSubrange(start..<(start + entry.source.count), with: newLines)
        write.lines = body
        return PlannedWrite(document: write)
    }

    func ordinal(of line: Int, hash: String, in note: String, heading: HeadingRef) -> Int {
        let file = TextFile(note)
        guard let found = file.resolve(heading) else { return 0 }
        let section = file.section(of: found)
        let mask = file.contentMask()
        let twins = section.body.filter { mask[$0] && SyncCore.lineHash(file.lines[$0].text) == hash }
        return twins.firstIndex(of: line) ?? 0
    }

    // MARK: Plan

    func replace(_ item: PlannerItem, with line: String, planner: Planner, day: VaultDay) -> PlannedWrite {
        var write = document(.replaceLine, path: config.dailyPath(day))
        write.heading = planner.heading
        write.lineHash = item.hash
        write.ordinal = item.ordinal
        write.newLine = line
        return PlannedWrite(document: write)
    }

    public func tick(_ item: PlannerItem, planner: Planner, day: VaultDay) -> PlannedWrite? {
        guard let parts = TaskLineParts(item.raw) else { return nil }
        return replace(item, with: parts.line(done: !item.done), planner: planner, day: day)
    }

    public func setTime(_ item: PlannerItem, startMinutes: Int?, endMinutes: Int?, planner: Planner, day: VaultDay) -> PlannedWrite? {
        guard var parts = TaskLineParts(item.raw) else { return nil }
        parts.time = startMinutes.map { TimePrefix.format(startMinutes: $0, endMinutes: endMinutes) }
        return replace(item, with: parts.line(), planner: planner, day: day)
    }

    public func editText(_ item: PlannerItem, text: String, planner: Planner, day: VaultDay) -> PlannedWrite? {
        guard var parts = TaskLineParts(item.raw) else { return nil }
        let cleaned = text.split(whereSeparator: \.isNewline).joined(separator: " ").trimmingCharacters(in: .whitespaces)
        guard !cleaned.isEmpty else { return nil }
        parts.text = cleaned
        return replace(item, with: parts.line(), planner: planner, day: day)
    }

    public func remove(_ item: PlannerItem, planner: Planner, day: VaultDay) -> PlannedWrite {
        var write = document(.removeLine, path: config.dailyPath(day))
        write.heading = planner.heading
        write.lineHash = item.hash
        write.ordinal = item.ordinal
        return PlannedWrite(document: write)
    }

    /// The line leaves the planner and lands under Soon, as the desk's wrap
    /// flow does. It never creates tomorrow's note a day early (V33 §17 #14).
    public func moveToSoon(_ item: PlannerItem, planner: Planner, day: VaultDay) -> [PlannedWrite] {
        guard let parts = TaskLineParts(item.raw) else { return [] }
        var append = document(.append, path: config.backlogFile)
        append.heading = HeadingRef(text: config.soonHeading, level: 2)
        append.lines = ["- [ ] " + parts.text]
        append.placement = .beforeChildren
        return [remove(item, planner: planner, day: day), PlannedWrite(document: append)]
    }

    /// A new line at the end of the planner, or of the group it was added from.
    public func addLine(_ text: String, group: PlannerGroup?, planner: Planner, day: VaultDay) -> PlannedWrite? {
        let cleaned = text.split(whereSeparator: \.isNewline).joined(separator: " ").trimmingCharacters(in: .whitespaces)
        guard !cleaned.isEmpty else { return nil }
        var write = document(.append, path: config.dailyPath(day))
        write.lines = ["- [ ] " + cleaned]
        if let group, group.name != nil, let heading = group.heading {
            write.heading = heading
            write.placement = .end
        } else {
            write.heading = planner.heading
            write.placement = .beforeChildren
        }
        write.createFromTemplate = day == today
        return PlannedWrite(document: write, seed: day == today ? dailySeed(day) : nil)
    }

    // MARK: Clip

    public struct Clip: Sendable {
        public var title: String
        public var url: String
        public var why: String
        public var quote: String?
        /// The readable text, already extracted; `nil` when the person turned
        /// "keep the article text" off or extraction found nothing.
        public var article: String?
        public var makeTask: Bool

        public init(title: String, url: String, why: String, quote: String? = nil, article: String? = nil, makeTask: Bool = false) {
            self.title = title
            self.url = url
            self.why = why
            self.quote = quote
            self.article = article
            self.makeTask = makeTask
        }
    }

    /// An inbox link note plus the readable text under `reference/clips/`
    /// (V33 §10). A second clip of the same page writes only the inbox note.
    public func clip(_ clip: Clip, taken: (String) -> Bool) -> (writes: [PlannedWrite], record: CaptureRecord) {
        let title = Slug.sanitizedTitle(clip.title.isEmpty ? clip.url : clip.title)
        let slug = Slug.make(title, fallback: "clip")
        let clipPath = "\(VaultConfig.clipsDir)/\(slug).md"
        var writes: [PlannedWrite] = []
        var keptText = false
        if let article = clip.article?.trimmingCharacters(in: .whitespacesAndNewlines), !article.isEmpty {
            keptText = true
            if !taken(clipPath) {
                var note = "---\nsource: clip\n"
                note += "url: \(clip.url)\n"
                note += "title: \(title)\n"
                note += "clipped: \(capturedAt)\n---\n\n# \(title)\n\n\(article)\n"
                var write = document(.create, path: clipPath)
                write.content = note
                writes.append(PlannedWrite(document: write))
            }
        }
        var body: [String] = []
        let why = clip.why.trimmingCharacters(in: .whitespacesAndNewlines)
        if !why.isEmpty { body.append(why) }
        body.append(clip.url)
        if keptText { body.append("[[clips/\(slug)]]") }
        if let quote = clip.quote?.trimmingCharacters(in: .whitespacesAndNewlines), !quote.isEmpty {
            body.append(quote.components(separatedBy: "\n").map { $0.isEmpty ? ">" : "> " + $0 }.joined(separator: "\n"))
        }
        let fields = InboxFields(title: title, body: body.joined(separator: "\n\n"), url: clip.url, kind: clip.makeTask ? "read" : nil)
        let (write, record) = inboxNote(fields, captureKind: .link, taken: taken)
        writes.append(write)
        return (writes, record)
    }
}

public enum Receipts {
    /// What became of a capture, from the vault itself: the inbox note still
    /// there, a triage-log line carrying its digest, or neither.
    public static func state(of record: CaptureRecord, exists: (String) -> Bool, log: [TriageLogLine]) -> ReceiptState {
        switch record.destination {
        case .today: return .addedToToday
        case .backlog: return .addedToBacklog
        case .inbox: break
        }
        if let line = log.last(where: { $0.digest == record.digest }) {
            if SyncCore.headingKey(line.destination).hasPrefix("discard") {
                return .discarded(day: line.day)
            }
            return .filed(destination: line.destination, day: line.day)
        }
        if let path = record.inboxPath, exists(path) {
            return .waiting
        }
        return .gone
    }
}
