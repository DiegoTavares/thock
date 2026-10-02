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
            if url.pathExtension == "json", url.deletingLastPathComponent().path != root.path {
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
}
