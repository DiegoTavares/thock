import XCTest
@testable import ThockKit

/// What arrives from outside the phone: pairing links, paths, envelopes.
final class SecurityTests: XCTestCase {
    let key = Data((0..<32).map { UInt8($0) })

    func link(code: String? = "K7MP-4QZX", key: String? = nil, backend: String? = "https://plus.thock.app", version: String? = "1") -> String {
        var components = URLComponents()
        components.scheme = "thock"
        components.host = "pair"
        components.queryItems = [("v", version), ("code", code), ("key", key ?? self.key.base64URL), ("backend", backend)]
            .compactMap { name, value in value.map { URLQueryItem(name: name, value: $0) } }
        return components.string ?? ""
    }

    // MARK: Pairing links

    func testAPairingLinkPastedWithSurroundingWhitespaceParses() throws {
        let parsed = try XCTUnwrap(PairingLink("  \n" + link() + "\n "))
        XCTAssertEqual(parsed, PairingLink(code: "K7MP-4QZX", key: key, backend: "https://plus.thock.app"))
        XCTAssertNotNil(PairingLink(link(version: nil)), "v defaults to 1")
    }

    func testMalformedPairingLinksAreRefused() {
        let refused = [
            "", "thock://pair", "thock://pair?", "thock:pair?v=1", "thock://unpair?" + (link().split(separator: "?").last.map(String.init) ?? ""),
            link().replacingOccurrences(of: "thock://", with: "https://"),
            link(version: "2"), link(version: ""), link(code: nil), link(code: ""), link(backend: nil), link(backend: ""),
            link(key: ""), link(key: "short"), link(key: Data(repeating: 1, count: 31).base64URL), link(key: Data(repeating: 1, count: 33).base64URL),
            link(key: String(repeating: "!", count: 43)), link(key: Data(repeating: 1, count: 32).hex),
        ]
        for text in refused {
            XCTAssertNil(PairingLink(text), text)
        }
    }

    func testAPairingLinkRoundTripsThroughItsURL() throws {
        for backend in ["https://plus.thock.app", "http://localhost:8080", "https://a.test/base path/", "http://[::1]:8080"] {
            let original = PairingLink(code: "ABCD-2345", key: key, backend: backend)
            XCTAssertEqual(PairingLink(original.url), original, backend)
        }
    }

    func testABackendWithItsOwnQueryRoundTrips() {
        let original = PairingLink(code: "ABCD-2345", key: key, backend: "https://a.test/?region=eu&tier=plus")
        XCTAssertEqual(PairingLink(original.url), original)
    }

    func testABackendIsHTTPSOrCleartextToALocalAddressOnly() {
        for backend in ["https://plus.thock.app", "http://localhost:8080", "http://127.0.0.1:8080", "http://[::1]:8080", "http://desk.local:8080"] {
            XCTAssertNotNil(PairingLink(link(backend: backend)), backend)
        }
        XCTExpectFailure("PairingLink accepts any backend, and AppModel.pair only checks it has a scheme (README: cleartext is for local addresses only)")
        for backend in ["http://plus.thock.app", "http://203.0.113.7:8080", "javascript:alert(1)", "file:///etc/passwd", "ftp://a.test", "plus.thock.app", "https://"] {
            XCTAssertNil(PairingLink(link(backend: backend)), backend)
        }
    }

    // MARK: Paths

    func testPathRules() {
        let longest = String(repeating: "a", count: 1021) + ".md"
        for path in ["a.md", "Daily/A.MD", "notes/café.md", "notes/ünïcødé 👩‍👩‍👧.md", "a..md", ".thock/config.toml", "deep/" + String(repeating: "x/", count: 50) + "a.csv", longest, ".thock/historyx/a.md"] {
            XCTAssertTrue(SyncCore.isSyncablePath(path), path)
        }
        let refused = [
            "/etc/passwd.md", "../a.md", "a/../../b.md", "a/..", "..", ".", "a/./b.md", "a//b.md", "a/", "/", "C:\\vault\\a.md", "a\\..\\b.md",
            "a\u{0}.md", "a\nb.md", "a\rb.md", "a\tb.md", "\u{7F}.md", "a.md.", ".md", "a.MD/", "a.png", "a.mdx", "a",
            ".git/config.json", ".thock/history/a.md", ".thock/cache/a.json", ".thock/sync/state.json", String(repeating: "a", count: 1022) + ".md",
        ]
        for path in refused {
            XCTAssertFalse(SyncCore.isSyncablePath(path), path.debugDescription)
        }
    }

    func testADecomposedPathIsRefused() {
        XCTAssertFalse(SyncCore.isSyncablePath("notes/Cafe\u{301}.md"))
    }

    func testTheLocalServerRefusesAWriteToAHostilePath() async throws {
        let harness = try Harness()
        try await harness.pair()
        let credential = String(data: harness.secrets.secret(SyncEngine.credentialName) ?? Data(), encoding: .utf8)
        for path in ["../escape.md", "/abs.md", ".git/config.json"] {
            let body = try JSONSerialization.data(withJSONObject: ["client_id": UUID().uuidString, "path": path, "base_version": 0, "payload": "AA=="])
            let reply = try await harness.backend.send(method: "POST", path: "/v1/vault/writes", query: [:], body: body, credential: credential)
            XCTAssertEqual(reply.status, 422, path)
        }
    }

    func testCaptureTitlesNeverEscapeTheInbox() throws {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        let writes = PhoneWrites(config: VaultConfig(), deviceID: "d", now: Date(timeIntervalSince1970: 1_790_975_640), calendar: calendar)
        for title in ["../../etc/passwd", "/absolute", "a/b\\c", "Cafe\u{301}", "👩‍👩‍👧 family", "\u{202E}gpj.md", "...", "CON", String(repeating: "long ", count: 100), "#", "\u{0}"] {
            let captured = try XCTUnwrap(writes.capture(blocks: Blocks.parse(title + "\n\nbody\n"), destination: .inbox, todayNote: nil, template: nil, taken: { _ in false }), title)
            for planned in captured.writes {
                let path = planned.document.path
                XCTAssertTrue(SyncCore.isSyncablePath(path), "\(title.debugDescription) → \(path.debugDescription)")
                XCTAssertTrue(path.hasPrefix("inbox/") && !path.dropFirst(6).contains("/"), path)
            }
        }
    }

    func testFolderSettingsAlwaysYieldSyncablePaths() {
        let day = VaultDay(year: 2026, month: 10, day: 2)
        for dir in ["daily", "daily/", "./daily", "/daily", "a/./b", "."] {
            let config = VaultConfig(config: "[daily]\ndir = \"\(dir)\"\n[weekly]\ndir = \"\(dir)\"\n")
            XCTAssertTrue(SyncCore.isSyncablePath(config.dailyPath(day)), "\(dir) → \(config.dailyPath(day))")
            XCTAssertTrue(SyncCore.isSyncablePath(config.weeklyPath(day)), "\(dir) → \(config.weeklyPath(day))")
        }
        for dir in ["inbox", "inbox/", "/inbox/", "Inbox Zero"] {
            XCTAssertTrue(SyncCore.isSyncablePath(VaultConfig(config: nil, inboxConfig: "dir = \"\(dir)\"\n").inboxDir + "/a.md"), dir)
        }
    }

    func testAnInboxFolderSpelledWithADotIsUsable() {
        XCTExpectFailure("VaultConfig trims only `/` and spaces from inbox.toml's dir, so `./inbox` or `.` gives paths the service refuses")
        for dir in ["./inbox", "inbox/./sub", "."] {
            let inboxDir = VaultConfig(config: nil, inboxConfig: "dir = \"\(dir)\"\n").inboxDir
            XCTAssertTrue(SyncCore.isSyncablePath(inboxDir + "/a.md"), "\(dir) → \(inboxDir)")
        }
    }

    // MARK: Envelopes

    func sealed(_ plaintext: String = "## Day planner\n- [ ] Call Ana\n", context: SealContext = .file(path: "daily/a.md", blobID: "aa")) throws -> Data {
        try SyncCore.seal(key: key, context: context, plaintext: Data(plaintext.utf8), nonce: Data(repeating: 3, count: 12))
    }

    func testAnEnvelopeHasItsLayoutAndIsDeterministicForANonce() throws {
        let envelope = try sealed("abc")
        XCTAssertEqual(envelope.prefix(4), Data("TVS1".utf8))
        XCTAssertEqual(envelope.count, 4 + 12 + 3 + 16)
        XCTAssertEqual(envelope.dropFirst(4).prefix(12), Data(repeating: 3, count: 12))
        XCTAssertEqual(try sealed("abc"), envelope)
        XCTAssertNotEqual(try SyncCore.seal(key: key, context: .write(clientID: "c"), plaintext: Data()), try SyncCore.seal(key: key, context: .write(clientID: "c"), plaintext: Data()))
        let empty = try SyncCore.seal(key: key, context: .write(clientID: "c"), plaintext: Data())
        XCTAssertEqual(try SyncCore.open(key: key, context: .write(clientID: "c"), envelope: empty), Data())
    }

    func testEveryTamperedByteIsCaught() throws {
        let envelope = try sealed()
        for index in 4..<envelope.count {
            var tampered = envelope
            tampered[index] ^= 0x01
            XCTAssertThrowsError(try SyncCore.open(key: key, context: .file(path: "daily/a.md", blobID: "aa"), envelope: tampered), "byte \(index)") {
                XCTAssertEqual($0 as? SealError, .authentication)
            }
        }
        var magic = envelope
        magic[0] ^= 0x01
        XCTAssertThrowsError(try SyncCore.open(key: key, context: .file(path: "daily/a.md", blobID: "aa"), envelope: magic)) {
            XCTAssertEqual($0 as? SealError, .badEnvelope)
        }
    }

    func testTruncatedOrExtendedEnvelopesAreRefused() throws {
        let envelope = try sealed()
        let context = SealContext.file(path: "daily/a.md", blobID: "aa")
        for length in [0, 3, 4, 16, 31] {
            XCTAssertThrowsError(try SyncCore.open(key: key, context: context, envelope: envelope.prefix(length)), "\(length)") {
                XCTAssertEqual($0 as? SealError, .badEnvelope)
            }
        }
        for length in [32, envelope.count - 1] {
            XCTAssertThrowsError(try SyncCore.open(key: key, context: context, envelope: envelope.prefix(length)), "\(length)") {
                XCTAssertEqual($0 as? SealError, .authentication)
            }
        }
        XCTAssertThrowsError(try SyncCore.open(key: key, context: context, envelope: envelope + Data([0])))
        // A slice that does not start at index 0 must still open.
        let padded = Data([9, 9]) + envelope
        XCTAssertEqual(try SyncCore.open(key: key, context: context, envelope: padded.dropFirst(2)), Data("## Day planner\n- [ ] Call Ana\n".utf8))
    }

    func testWrongKeysAndContextsAreRefused() throws {
        let envelope = try sealed()
        for wrong in [Data(), Data(repeating: 0, count: 16), Data(repeating: 0, count: 33)] {
            XCTAssertThrowsError(try SyncCore.open(key: wrong, context: .file(path: "daily/a.md", blobID: "aa"), envelope: envelope)) {
                XCTAssertEqual($0 as? SealError, .badKey)
            }
            XCTAssertThrowsError(try SyncCore.seal(key: wrong, context: .write(clientID: "c"), plaintext: Data())) {
                XCTAssertEqual($0 as? SealError, .badKey)
            }
        }
        var flipped = key
        flipped[31] ^= 0x80
        XCTAssertThrowsError(try SyncCore.open(key: flipped, context: .file(path: "daily/a.md", blobID: "aa"), envelope: envelope))
        for context in [SealContext.write(clientID: "daily/a.md"), .file(path: "daily/a.md", blobID: "AA"), .file(path: "Daily/a.md", blobID: "aa"), .file(path: "daily/a.md\u{0}aa", blobID: "")] {
            XCTAssertThrowsError(try SyncCore.open(key: key, context: context, envelope: envelope), "\(context)")
        }
        XCTAssertThrowsError(try SyncCore.seal(key: key, context: .write(clientID: "c"), plaintext: Data(), nonce: Data(repeating: 0, count: 8)))
    }

    func testTheKeyCheckAndEncodings() {
        let check = SyncCore.keyCheck(key: key)
        XCTAssertEqual(check.count, 32)
        XCTAssertTrue(check.allSatisfy { $0.isHexDigit && !$0.isUppercase })
        XCTAssertNotEqual(SyncCore.keyCheck(key: Data(repeating: 0, count: 32)), check)
        XCTAssertFalse(SyncCore.sha256Hex(key).hasPrefix(check), "the check must not be a plain hash of the key")

        for length in 0..<40 {
            let data = Data((0..<length).map { UInt8(($0 * 37 + 11) % 256) })
            XCTAssertEqual(Data(base64URL: data.base64URL), data)
            XCTAssertFalse(data.base64URL.contains { "+/=".contains($0) })
            XCTAssertEqual(Data(hex: data.hex), data)
        }
        XCTAssertNil(Data(base64URL: "a!b"))
        XCTAssertNil(Data(hex: "abc"))
        XCTAssertNil(Data(hex: "zz"))
    }
}
