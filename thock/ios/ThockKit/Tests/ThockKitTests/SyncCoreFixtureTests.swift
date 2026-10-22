import XCTest
@testable import ThockKit

/// Runs every fixture case in the V34 API §9.2 format: the phone-authored
/// ones bundled with these tests and, when the checkout has them, the desk's
/// under `crates/thock-sync-core/fixtures/v1`.
final class SyncCoreFixtureTests: XCTestCase {
    struct Case: Decodable {
        var name: String
        var before: String?
        var seed: String?
        var write: WriteDocument?
        var after: String
        var outcome: String
        var effect_present_before: Bool?
        var effect_present_after: Bool?
    }

    static func fixtureRoots() -> [URL] {
        var roots: [URL] = []
        if let bundled = Bundle.module.resourceURL?.appendingPathComponent("Fixtures/v1") {
            roots.append(bundled)
        }
        // `THOCK_SYNC_FIXTURES=/path/to/fixtures/v1 swift test` runs a corpus
        // that is not in this checkout, such as one from another branch.
        if let extra = ProcessInfo.processInfo.environment["THOCK_SYNC_FIXTURES"], FileManager.default.fileExists(atPath: extra) {
            roots.append(URL(fileURLWithPath: extra))
        }
        var directory = URL(fileURLWithPath: #filePath)
        for _ in 0..<8 {
            directory.deleteLastPathComponent()
            let desk = directory.appendingPathComponent("crates/thock-sync-core/fixtures/v1")
            if FileManager.default.fileExists(atPath: desk.path) {
                roots.append(desk)
                break
            }
        }
        return roots
    }

    func caseFiles(in root: URL) -> [URL] {
        let enumerator = FileManager.default.enumerator(at: root, includingPropertiesForKeys: nil)
        var files: [URL] = []
        while let url = enumerator?.nextObject() as? URL {
            // `reminder_index/` holds data-only reminder cases, not writes.
            if url.pathExtension == "json", url.deletingLastPathComponent().path != root.path,
               url.deletingLastPathComponent().lastPathComponent != "reminder_index" {
                files.append(url)
            }
        }
        return files.sorted { $0.path < $1.path }
    }

    func testEveryCase() throws {
        var count = 0
        for root in Self.fixtureRoots() {
            for url in caseFiles(in: root) {
                let label = url.deletingLastPathComponent().lastPathComponent + "/" + url.lastPathComponent
                let fixture = try JSONDecoder().decode(Case.self, from: Data(contentsOf: url))
                count += 1
                guard let write = fixture.write else {
                    let before = fixture.before ?? ""
                    XCTAssertEqual(TextFile(before).text, before, "\(label): roundtrip")
                    var empty = WriteDocument(clientID: "roundtrip", kind: .append, path: "x.md", madeAt: "", deviceID: "")
                    empty.heading = nil
                    XCTAssertEqual(SyncCore.apply(existing: before, write: empty).text, before, "\(label): an empty append hands the note back")
                    continue
                }
                try write.validate()
                XCTAssertEqual(try WriteDocument.parse(write.json()), write, "\(label): write survives its own JSON")
                if let expected = fixture.effect_present_before, let before = fixture.before {
                    XCTAssertEqual(SyncCore.effectPresent(content: before, write: write), expected, "\(label): effect present before")
                }
                let applied = SyncCore.apply(existing: fixture.before, write: write, seed: fixture.seed)
                XCTAssertEqual(applied.text, fixture.after, "\(label): text")
                XCTAssertEqual(applied.outcome.rawValue, fixture.outcome, "\(label): outcome")
                if let expected = fixture.effect_present_after {
                    XCTAssertEqual(SyncCore.effectPresent(content: fixture.after, write: write), expected, "\(label): effect present after")
                }
                let again = SyncCore.apply(existing: fixture.after, write: write, seed: fixture.seed)
                XCTAssertEqual(again.text, fixture.after, "\(label): applying twice equals applying once")
                XCTAssertEqual(again.outcome, .noop, "\(label): second application is a no-op")
            }
        }
        XCTAssertGreaterThan(count, 30)
    }

    func testHashVectors() throws {
        struct Vector: Decodable {
            var line: String?
            var line_hash: String?
            var text: String?
            var heading_key: String?
            var body: [String]?
            var section_hash: String?
        }
        for root in Self.fixtureRoots() {
            let url = root.appendingPathComponent("hashes.json")
            guard FileManager.default.fileExists(atPath: url.path) else { continue }
            for vector in try JSONDecoder().decode([Vector].self, from: Data(contentsOf: url)) {
                if let line = vector.line {
                    XCTAssertEqual(SyncCore.lineHash(line), vector.line_hash, "line_hash of \(line)")
                }
                if let text = vector.text {
                    XCTAssertEqual(SyncCore.headingKey(text), vector.heading_key, "heading_key of \(text)")
                }
                if let body = vector.body {
                    XCTAssertEqual(SyncCore.sectionHash(lines: body), vector.section_hash, "section_hash of \(body)")
                }
            }
        }
    }

    func testEnvelopeVectors() throws {
        struct Vector: Decodable {
            var key: String
            var nonce: String?
            var context: [String: String]?
            var plaintext: String?
            var envelope: String?
            var content_hash: String?
            var key_check: String?
        }
        var sealed = 0
        for root in Self.fixtureRoots() {
        for vector in try JSONDecoder().decode([Vector].self, from: Data(contentsOf: root.appendingPathComponent("envelope.json"))) {
            let key = try XCTUnwrap(Data(hex: vector.key))
            if let check = vector.key_check {
                XCTAssertEqual(SyncCore.keyCheck(key: key), check)
                continue
            }
            let context = try XCTUnwrap(vector.context)
            let seal: SealContext = context["kind"] == "file"
                ? .file(path: context["path"] ?? "", blobID: context["blob_id"] ?? "")
                : .write(clientID: context["client_id"] ?? "")
            let plaintext = try XCTUnwrap(Data(base64Encoded: vector.plaintext ?? ""))
            let envelope = try XCTUnwrap(Data(base64Encoded: vector.envelope ?? ""))
            let nonce = try XCTUnwrap(Data(hex: vector.nonce ?? ""))
            XCTAssertEqual(try SyncCore.seal(key: key, context: seal, plaintext: plaintext, nonce: nonce), envelope)
            XCTAssertEqual(try SyncCore.open(key: key, context: seal, envelope: envelope), plaintext)
            XCTAssertEqual(SyncCore.contentHash(envelope: envelope), vector.content_hash)
            let other: SealContext = context["kind"] == "file" ? .file(path: (context["path"] ?? "") + "x", blobID: context["blob_id"] ?? "") : .write(clientID: (context["client_id"] ?? "") + "x")
            XCTAssertThrowsError(try SyncCore.open(key: key, context: other, envelope: envelope))
            sealed += 1
        }
        }
        XCTAssertGreaterThanOrEqual(sealed, 2)
    }

    func testPathVectors() throws {
        struct Vectors: Decodable {
            var syncable: [String]
            var refused: [String]
        }
        var checked = 0
        for root in Self.fixtureRoots() {
            let url = root.appendingPathComponent("paths.json")
            guard FileManager.default.fileExists(atPath: url.path) else { continue }
            let vectors = try JSONDecoder().decode(Vectors.self, from: Data(contentsOf: url))
            for path in vectors.syncable {
                XCTAssertTrue(SyncCore.isSyncablePath(path), path.debugDescription)
            }
            for path in vectors.refused {
                XCTAssertFalse(SyncCore.isSyncablePath(path), path.debugDescription)
            }
            checked += vectors.syncable.count + vectors.refused.count
        }
        XCTAssertGreaterThan(checked, 0)
    }

    func testBlobCannotMoveToAnotherPathOrBlob() throws {
        let key = Data(repeating: 7, count: 32)
        let envelope = try SyncCore.seal(key: key, context: .file(path: "daily/a.md", blobID: "aa"), plaintext: Data("x".utf8))
        XCTAssertThrowsError(try SyncCore.open(key: key, context: .file(path: "daily/b.md", blobID: "aa"), envelope: envelope))
        XCTAssertThrowsError(try SyncCore.open(key: key, context: .file(path: "daily/a.md", blobID: "bb"), envelope: envelope))
        XCTAssertThrowsError(try SyncCore.open(key: Data(repeating: 8, count: 32), context: .file(path: "daily/a.md", blobID: "aa"), envelope: envelope))
        XCTAssertEqual(try SyncCore.open(key: key, context: .file(path: "daily/a.md", blobID: "aa"), envelope: envelope), Data("x".utf8))
    }

    func testSyncablePaths() {
        for path in ["daily/2026-10-02.md", ".thock/config.toml", "routines/inbox/triage-policy.md", "a b/c.TXT", "data.csv"] {
            XCTAssertTrue(SyncCore.isSyncablePath(path), path)
        }
        for path in ["/daily/a.md", "daily//a.md", "daily/../a.md", "./a.md", "a.png", "noextension", ".thock/history/x.md", ".git/config.json", ".thock/sync/state.json", "daily/a.md/", ""] {
            XCTAssertFalse(SyncCore.isSyncablePath(path), path)
        }
    }

    func testMalformedWritesAreRejectedBeforeApplication() {
        XCTAssertThrowsError(try WriteDocument.parse(#"{"v":2,"client_id":"a","kind":"create","path":"x.md","content":"x"}"#))
        XCTAssertThrowsError(try WriteDocument.parse(#"{"v":1,"client_id":"a","kind":"replace_line","path":"x.md"}"#))
        XCTAssertThrowsError(try WriteDocument.parse(#"{"v":1,"client_id":"a","kind":"append","path":"x.md","heading":null,"lines":["a\nb"]}"#))
        XCTAssertNoThrow(try WriteDocument.parse(#"{"v":1,"client_id":"a","kind":"append","path":"x.md","heading":null,"lines":["a"],"unknown":true}"#))
    }

    // MARK: Reminder vectors (V45 §11)

    struct DailyVector: Decodable {
        var dir: String
        var filename: String

        var notes: DailyNotes { DailyNotes(dir: dir, filename: filename) }
    }

    func reminderFiles(_ name: String) -> [URL] {
        Self.fixtureRoots().map { $0.appendingPathComponent(name) }.filter { FileManager.default.fileExists(atPath: $0.path) }
    }

    /// The note split on `\n`, a trailing `\r` belonging to the ending. By
    /// bytes: `\r\n` is one `Character`, so a `Character` split misses it.
    static func lines(_ text: String) -> [String] {
        Array(text.utf8).split(separator: UInt8(ascii: "\n"), omittingEmptySubsequences: false).map { line in
            Bytes.string(line.last == UInt8(ascii: "\r") ? line.dropLast() : line)
        }
    }

    func testReminderVectors() throws {
        struct Scope: Decodable {
            var templates: [String]
            var counted: [String]
            var ignored: [String]
        }
        struct Expected: Decodable {
            var line: Int
            var id: String?
            var token: String?
            var when: String?
            var struck: Bool
            var task: String?
            var due: String?
            var state: String
            var mirror: Bool
            var text: String
        }
        struct Case: Decodable {
            var name: String
            var path: String
            var daily: DailyVector
            var day_headings: [String]?
            var note: String
            var reminders: [Expected]
        }
        enum Mark: Decodable, Equatable {
            case named(String)
            case moved(String)

            init(from decoder: Decoder) throws {
                if let name = try? decoder.singleValueContainer().decode(String.self) {
                    self = .named(name)
                } else {
                    self = .moved(try decoder.singleValueContainer().decode([String: String].self)["moved"] ?? "")
                }
            }
        }
        struct ExpectedMirror: Decodable {
            var checked: Bool
            var time: String?
            var text: String
            var mark: Mark?
            var carried: String?
            var note: String
            var id: String
        }
        struct Mirror: Decodable {
            var line: String
            var mirror: ExpectedMirror?
        }
        struct Cut: Decodable {
            var text: String
            var max: Int
            var cut: String
        }
        struct Vectors: Decodable {
            var scope: [Scope]
            var cases: [Case]
            var mirrors: [Mirror]
            var texts: [Cut]
        }
        func whenText(_ when: ReminderWhen) -> String {
            switch when {
            case .date(let date): return date.iso
            case .dateTime(let date, let time): return "\(date.iso) \(time)"
            case .time(let time): return time.description
            }
        }

        let urls = reminderFiles("reminders.json")
        XCTAssertFalse(urls.isEmpty)
        for url in urls {
            let vectors = try JSONDecoder().decode(Vectors.self, from: Data(contentsOf: url))
            for scope in vectors.scope {
                for path in scope.counted {
                    XCTAssertTrue(Reminders.isReminderPath(path, templatePaths: scope.templates), "\(path) counts")
                }
                for path in scope.ignored {
                    XCTAssertFalse(Reminders.isReminderPath(path, templatePaths: scope.templates), "\(path) is ignored")
                }
            }

            XCTAssertGreaterThanOrEqual(vectors.cases.count, 30)
            for vector in vectors.cases {
                let label = vector.name
                let config = ReminderConfig(daily: vector.daily.notes, dayHeadings: vector.day_headings ?? [Reminders.defaultDayHeading])
                let lines = Self.lines(vector.note)
                let found = Reminders.parseNote(vector.note, path: vector.path, config: config)
                XCTAssertEqual(found.map(\.line), vector.reminders.map(\.line), "\(label): lines")
                for (appearance, expected) in zip(found, vector.reminders) {
                    let line = lines[appearance.line]
                    let reminder = appearance.reminder
                    XCTAssertEqual(reminder.id, expected.id, "\(label): id")
                    XCTAssertEqual(reminder.token?.text(in: line), expected.token, "\(label): token")
                    XCTAssertEqual(reminder.token.map { whenText($0.when) }, expected.when, "\(label): when")
                    XCTAssertEqual(reminder.token?.struck ?? false, expected.struck, "\(label): struck")
                    XCTAssertEqual(reminder.task.map { $0 == .open ? "open" : "done" }, expected.task, "\(label): task")
                    XCTAssertEqual(reminder.due?.description, expected.due, "\(label): due")
                    XCTAssertEqual(reminder.state.rawValue, expected.state, "\(label): state")
                    XCTAssertEqual(appearance.mirror, expected.mirror, "\(label): mirror")
                    XCTAssertEqual(Reminders.reminderText(line), expected.text, "\(label): text")
                    if !vector.note.contains("\n") {
                        XCTAssertEqual(Reminders.parseLine(line, day: config.daily.day(of: vector.path)), reminder, "\(label): parseLine agrees")
                    }
                }
            }

            for vector in vectors.mirrors {
                let parsed = MirrorLine(parsing: vector.line)
                guard let expected = vector.mirror else {
                    XCTAssertNil(parsed, "\(vector.line) is not a mirror line")
                    continue
                }
                guard let parsed else {
                    XCTFail("\(vector.line) is a mirror line")
                    continue
                }
                let mark: MirrorMark?
                switch expected.mark {
                case nil: mark = nil
                case .named("dropped")?: mark = .dropped
                case .named("removed")?: mark = .removed
                case .named(let other)?: XCTFail("unknown mark \(other)"); mark = nil
                case .moved(let date)?: mark = VaultDay(iso: date).map(MirrorMark.moved)
                }
                let wanted = MirrorLine(
                    checked: expected.checked, time: expected.time.flatMap(ClockTime.init), text: expected.text, mark: mark,
                    carried: expected.carried.flatMap(ReminderDue.init), note: expected.note, id: expected.id
                )
                XCTAssertEqual(parsed, wanted, vector.line)
                XCTAssertEqual(parsed.description, vector.line, "mirror lines round-trip")
            }

            for vector in vectors.texts {
                XCTAssertEqual(Reminders.cutText(vector.text, maxCharacters: vector.max), vector.cut, vector.text)
            }
        }
    }

    func testDailyPathVectors() throws {
        struct Vector: Decodable {
            var daily: DailyVector
            var path: String
            var day: String?
        }
        let urls = reminderFiles("daily_paths.json")
        XCTAssertFalse(urls.isEmpty)
        for url in urls {
            let vectors = try JSONDecoder().decode([Vector].self, from: Data(contentsOf: url))
            XCTAssertGreaterThanOrEqual(vectors.count, 20)
            for vector in vectors {
                XCTAssertEqual(vector.daily.notes.day(of: vector.path), vector.day.flatMap(VaultDay.init(iso:)), "\(vector.daily.filename) \(vector.path)")
            }
        }
    }

    func testReminderEditVectors() throws {
        struct Edit: Decodable {
            var kind: String
            var due: String?
            var id: String?
        }
        struct Vector: Decodable {
            var name: String
            var path: String
            var daily: DailyVector
            var line: String
            var edit: Edit
            var after: String?
        }
        let urls = reminderFiles("reminder_edits.json")
        XCTAssertFalse(urls.isEmpty)
        for url in urls {
            let vectors = try JSONDecoder().decode([Vector].self, from: Data(contentsOf: url))
            XCTAssertGreaterThanOrEqual(vectors.count, 50)
            for vector in vectors {
                let day = vector.daily.notes.day(of: vector.path)
                let edit = vector.edit
                func due() throws -> ReminderDue {
                    try XCTUnwrap(edit.due.flatMap(ReminderDue.init), "\(vector.name): due")
                }
                let id = edit.id ?? ""
                let after: String?
                switch edit.kind {
                case "close": after = Reminders.close(vector.line)
                case "reopen": after = Reminders.reopen(vector.line)
                case "rewrite": after = Reminders.rewrite(vector.line, due: try due(), homeDay: day)
                case "drop": after = Reminders.dropToken(vector.line)
                case "attach": after = Reminders.attach(vector.line, due: try due(), id: id, homeDay: day)
                case "add_id": after = Reminders.addID(vector.line, id: id)
                default:
                    XCTFail("\(vector.name): unknown edit \(edit.kind)")
                    continue
                }
                XCTAssertEqual(after, vector.after, vector.name)
                if edit.kind == "add_id", let after {
                    XCTAssertEqual(SyncCore.lineHash(after), SyncCore.lineHash(vector.line), "\(vector.name): an id never changes a line's identity")
                }
            }
        }
    }

    /// The index cases read as the desk's runner reads them: each entry's
    /// home line found by id, with its state, due and group. `ReminderIndex`
    /// (V45 P1) reads the same cases its own way.
    func testReminderIndexDerivations() throws {
        struct Headings: Decodable {
            var open: [String]
            var done: [String]
            var day: [String]
        }
        struct Expected: Decodable {
            var id: String
            var home: String
            var line: Int?
            var text: String?
            var due: String?
            var rings_at: String?
            var state: String
            var group: String?
            var stamp: [String: String]?
        }
        struct Case: Decodable {
            var name: String
            var now: String
            var index: String
            var headings: Headings
            var daily: DailyVector
            var date_only_at: String
            var files: [String: String]
            var reminders: [Expected]
        }
        var checked = 0
        for root in Self.fixtureRoots() {
            let folder = root.appendingPathComponent("reminder_index")
            guard let names = try? FileManager.default.contentsOfDirectory(atPath: folder.path) else { continue }
            for name in names.sorted() where name.hasSuffix(".json") {
                let vector = try JSONDecoder().decode(Case.self, from: Data(contentsOf: folder.appendingPathComponent(name)))
                let config = ReminderConfig(daily: vector.daily.notes, dayHeadings: vector.headings.day)
                let today = try XCTUnwrap(VaultDay(iso: String(vector.now.prefix(10))))
                let dateOnlyAt = try XCTUnwrap(ClockTime(vector.date_only_at))
                let indexText = vector.files[vector.index] ?? ""
                let entries = Reminders.parseNote(indexText, path: vector.index, config: config).filter { $0.reminder.id != nil }
                XCTAssertEqual(entries.compactMap(\.reminder.id), vector.reminders.map(\.id), "\(vector.name): entries")
                for expected in vector.reminders {
                    let label = "\(vector.name) \(expected.id)"
                    let homeText = vector.files[expected.home]
                    let homes = homeText.map { Reminders.parseNote($0, path: expected.home, config: config) }?
                        .filter { !$0.mirror && $0.reminder.id == expected.id } ?? []
                    guard let home = homes.first(where: { $0.reminder.token != nil }) ?? homes.first, let homeText else {
                        XCTAssertEqual(expected.state, expected.stamp?["kind"] == "removed" ? "removed" : "missing", label)
                        XCTAssertNil(expected.line, label)
                        continue
                    }
                    let reminder = home.reminder
                    XCTAssertEqual(home.line, expected.line, "\(label): home line")
                    XCTAssertEqual(Reminders.reminderText(Self.lines(homeText)[home.line]), expected.text, "\(label): text")
                    XCTAssertEqual(reminder.due?.description, expected.due, "\(label): due")
                    let rings = reminder.due.map { due -> String in
                        let moment = due.ringsAt(dateOnlyAt: dateOnlyAt)
                        return "\(moment.date.iso)T\(moment.time)"
                    }
                    XCTAssertEqual(rings, expected.rings_at, "\(label): rings at")
                    XCTAssertEqual(reminder.state.rawValue, expected.state, "\(label): state")
                    XCTAssertEqual(Reminders.group(state: reminder.state, due: reminder.due, today: today)?.rawValue, expected.group, "\(label): group")
                    checked += 1
                }
            }
        }
        XCTAssertGreaterThan(checked, 0)
    }
}
