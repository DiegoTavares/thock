import XCTest
@testable import ThockKit

/// A model that says what the test tells it to and remembers what it was sent.
final class ScriptedModel: ChatTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var replies: [Result<ChatReply, Error>]
    private(set) var requests: [ChatRequest] = []

    init(_ replies: [Result<ChatReply, Error>]) {
        self.replies = replies
    }

    func complete(_ request: ChatRequest) async throws -> ChatReply {
        try lock.withLock {
            requests.append(request)
            guard !replies.isEmpty else { return ChatReply(text: "") }
            return try replies.removeFirst().get()
        }
    }
}

final class AskTests: XCTestCase {
    let now = Date(timeIntervalSince1970: 1_791_036_120) // 2026-10-03 14:02 UTC

    var calendar: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }

    func vault(_ notes: [String: String]) throws -> VaultSession {
        let store = try VaultStore(url: nil)
        store.setMeta("vault_id", "vault")
        store.setMeta("device_id", "c41a9e0d7b2f4a61")
        for (index, note) in notes.sorted(by: { $0.key < $1.key }).enumerated() {
            try store.applySnapshot(path: note.key, version: index + 1, content: note.value, contentHash: "hash", blobID: "blob")
        }
        return VaultSession(store: store, calendar: calendar)
    }

    let notes = [
        "daily/2026-09-11.md": "# Friday\n\n## Journal\n\nQuiet day. Planned the Maestro work.\n",
        "daily/2026-09-12.md": "# Saturday\n\n## Journal\n\nDeployed the Maestro fix after lunch; Rui reviewed it.\n",
        "daily/2026-03-14.md": "# Saturday\n\n## Journal\n\nA migração do banco ficou com a Ana.\n",
        "projects/maestro.md": "# Maestro\n\nThe scheduler rewrite.\n",
        "memory/index.md": "# What Thock has learned\n\n- **Ana**, your manager. → people/ana.md\n",
        ".thock/config.toml": "schema = 1\n[language]\nname = \"Portuguese (Brazil)\"\ntag = \"pt-BR\"\n[memory]\nindex_lines = 2\n",
        "reference/data.csv": "maestro,1\n",
    ]

    func grant(status: String = "active", used: Int = 10) -> AgentGrant {
        AgentGrant(status: status, allowanceUnits: 100, usedUnits: used, remainingUnits: 100 - used, warnAtPercent: 80, cycleEndsAt: nil,
                   gateway: .init(provider: "openrouter", baseURL: "https://gateway.test/v1", apiKey: "key", models: .init(default: "model/default", fast: "model/fast")))
    }

    func agent(_ session: VaultSession, _ model: ScriptedModel, grant: AgentGrant? = nil) -> AskAgent {
        let grant = grant ?? self.grant()
        return AskAgent(session: session, transport: model, grant: { grant })
    }

    func call(_ id: String, _ name: String, _ arguments: [String: Any]) -> ToolCall {
        let data = try! JSONSerialization.data(withJSONObject: arguments)
        return ToolCall(id: id, name: name, arguments: String(decoding: data, as: UTF8.self))
    }

    // MARK: Search

    func testSearchRanksFoldsAndFilters() throws {
        let store = try vault(notes).store
        let hits = store.search("Maestro deploy")
        XCTAssertEqual(hits.first?.path, "daily/2026-09-12.md")
        XCTAssertTrue(hits.contains { $0.path == "projects/maestro.md" })
        XCTAssertFalse(hits.contains { $0.path.hasSuffix(".csv") })
        XCTAssertTrue(hits[0].excerpt.contains("Deployed the Maestro fix"))
        XCTAssertEqual(store.search("migracao").map(\.path), ["daily/2026-03-14.md"])
        XCTAssertEqual(store.search("maestro", folder: "projects/").map(\.path), ["projects/maestro.md"])
        XCTAssertEqual(store.search("\" OR * NEAR(").count, 0)
        XCTAssertEqual(store.search("zebra").count, 0)
    }

    func testTheIndexFollowsEveryChange() throws {
        let session = try vault(notes)
        let store = session.store
        try store.applySnapshot(path: "daily/2026-09-12.md", version: 20, content: "# Saturday\n\nNothing shipped.\n", contentHash: "h", blobID: "b")
        XCTAssertFalse(store.search("deployed").contains { $0.path == "daily/2026-09-12.md" })
        XCTAssertEqual(store.search("shipped").map(\.path), ["daily/2026-09-12.md"])

        try store.applyTombstone(path: "projects/maestro.md", version: 21)
        XCTAssertFalse(store.search("scheduler").contains { $0.path == "projects/maestro.md" })

        try session.remember("- 2026-10-03 · Rui is the contractor on Maestro.", now: now)
        XCTAssertEqual(store.search("contractor").map(\.path), ["memory/inbox.md"])

        store.wipe()
        XCTAssertEqual(store.search("maestro").count, 0)
    }

    func testAStoreFromBeforeTheIndexIsIndexedOnOpen() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("ask-\(UUID().uuidString).sqlite")
        defer { try? FileManager.default.removeItem(at: url) }
        do {
            let old = try Database(path: url.path)
            try old.execute("CREATE TABLE files (path TEXT PRIMARY KEY, version INTEGER NOT NULL DEFAULT 0, snapshot TEXT, content TEXT NOT NULL, content_hash TEXT, blob_id TEXT)")
            try old.execute("INSERT INTO files (path, content) VALUES ('daily/2026-09-12.md', 'Deployed the Maestro fix.')")
        }
        let store = try VaultStore(url: url)
        XCTAssertEqual(store.search("maestro").map(\.path), ["daily/2026-09-12.md"])
    }

    // MARK: Tools

    func testReadCutsLongNotesAndContinues() throws {
        let long = (1...450).map { "line \($0)" }.joined(separator: "\n")
        let tools = AskTools(session: try vault(["long.md": long]), now: now)
        let first = tools.run(name: "read", arguments: #"{"path": "/long.md"}"#)
        XCTAssertEqual(first.source, "long.md")
        XCTAssertTrue(first.result.hasPrefix("line 1\n"))
        XCTAssertTrue(first.result.hasSuffix("[50 more lines. Read again with from_line 401 to continue.]"))
        let rest = tools.run(name: "read", arguments: #"{"path": "long.md", "from_line": 401}"#)
        XCTAssertEqual(rest.result, (401...450).map { "line \($0)" }.joined(separator: "\n"))

        let missing = tools.run(name: "read", arguments: #"{"path": "nope.md"}"#)
        XCTAssertNil(missing.source)
        XCTAssertTrue(missing.result.hasPrefix("There is no note at nope.md."))
        XCTAssertTrue(tools.run(name: "read", arguments: "not json").result.contains("could not be read"))
        XCTAssertTrue(tools.run(name: "bash", arguments: "{}").result.contains("no tool called bash"))
    }

    func testListShowsFoldersThenNotesAndCapsLongFolders() throws {
        var many = notes
        for day in 1...230 {
            many["log/\(String(format: "%04d", day)).md"] = "x"
        }
        let tools = AskTools(session: try vault(many), now: now)
        let top = tools.run(name: "list", arguments: "{}").result
        XCTAssertTrue(top.contains("daily/ (3 notes)"))
        XCTAssertFalse(top.contains(".thock"))
        XCTAssertEqual(tools.run(name: "list", arguments: #"{"folder": "daily"}"#).result,
                       "daily/2026-03-14.md\ndaily/2026-09-11.md\ndaily/2026-09-12.md")
        let log = tools.run(name: "list", arguments: #"{"folder": "log"}"#).result.components(separatedBy: "\n")
        XCTAssertEqual(log.count, 201)
        XCTAssertEqual(log.first, "[30 earlier notes not shown, from log/0001.md]")
        XCTAssertEqual(log.last, "log/0230.md")
        XCTAssertEqual(tools.run(name: "list", arguments: #"{"folder": "nowhere"}"#).result, "There is nothing under nowhere.")
    }

    func testAppendOnlyReachesTheMemoryInbox() throws {
        let session = try vault(notes)
        let tools = AskTools(session: session, now: now)
        let refused = tools.run(name: "append", arguments: #"{"path": "backlog.md", "text": "- [ ] Buy milk"}"#)
        XCTAssertTrue(refused.result.contains("Nothing was written"))
        XCTAssertNil(session.store.content("backlog.md"))
        XCTAssertTrue(session.store.pending().isEmpty)

        let kept = tools.run(name: "append", arguments: #"{"path": "memory/inbox.md", "text": "My manager is Ana now.\n- 2026-10-01 · 1:1 is on Thursdays."}"#)
        XCTAssertEqual(kept.activity, "Noted one thing for later")
        XCTAssertEqual(session.store.content("memory/inbox.md"), "- 2026-10-03 · My manager is Ana now.\n- 2026-10-01 · 1:1 is on Thursdays.\n")
        let queued = session.store.pending()
        XCTAssertEqual(queued.map(\.document.path), ["memory/inbox.md"])
        XCTAssertEqual(queued.first?.document.kind, .append)
        XCTAssertNil(queued.first?.document.heading)
    }

    // MARK: Prompt

    func testTheBundledDeskPromptIsTheDesksOwn() throws {
        let bundled = AskPrompt.resource("SYSTEM")
        XCTAssertFalse(bundled.isEmpty)
        for heading in AskPrompt.sharedHeadings {
            XCTAssertNotNil(AskPrompt.section(heading, of: bundled), heading)
        }
        var directory = URL(fileURLWithPath: #filePath)
        for _ in 0..<8 {
            directory.deleteLastPathComponent()
            let desk = directory.appendingPathComponent("crates/thock/assets/hosted-agent/SYSTEM.md")
            if let text = try? String(contentsOf: desk, encoding: .utf8) {
                XCTAssertEqual(bundled, text, "Copy crates/thock/assets/hosted-agent/SYSTEM.md over Ask/Prompts/SYSTEM.md")
                return
            }
        }
    }

    func testThePromptCarriesTheSharedSectionsAndTheVaultsWords() throws {
        var notes = notes
        notes["AGENTS.md"] = "# This vault\n\nDaily notes live in `daily/`.\n"
        notes["profile.md"] = "# About me\n\nI lead the platform team.\n"
        let prompt = AskPrompt.system(session: try vault(notes), now: now)
        XCTAssertTrue(prompt.hasPrefix("You are the Thock Agent, answering from this person's phone."))
        for expected in ["## Answering on the phone", "## How you speak", "## What you remember", "## Language\n\nSpeak and write",
                         "# Right now", "## The vault's own instructions", "Daily notes live in `daily/`.", "## Who this person is", "I lead the platform team."] {
            XCTAssertTrue(prompt.contains(expected), expected)
        }
        // The desk's tools and rituals sections are not the phone's.
        XCTAssertFalse(prompt.contains("`bash`"))
        XCTAssertFalse(prompt.contains("## Rituals and Routines"))
    }

    func testTheContextBlockSaysWhatIsTrueNow() throws {
        let context = AskPrompt.context(session: try vault(notes), now: now)
        XCTAssertTrue(context.contains("Today is Saturday, 3 October 2026 (2026-10-03), week 2026-W40. The time is 14:02."))
        XCTAssertTrue(context.contains("- Today's note: `daily/2026-10-03.md` (not written yet)."))
        XCTAssertTrue(context.contains("- Tasks: `backlog.md`, under the headings `Soon`, `Someday` and `Completed`."))
        XCTAssertTrue(context.contains("This vault is set to **Portuguese (Brazil)** (`pt-BR`)."))
        // The index is three lines and the vault caps it at two.
        XCTAssertTrue(context.contains("# What Thock has learned\n\n\nThe rest of this page is over its cap"))
        XCTAssertFalse(context.contains("your manager"))

        let bare = AskPrompt.context(session: try vault(["daily/2026-10-03.md": "# Today\n"]), now: now)
        XCTAssertTrue(bare.contains("- Today's note: `daily/2026-10-03.md`."))
        XCTAssertTrue(bare.contains("No language has been set for this vault"))
        XCTAssertTrue(bare.contains("Nothing yet: no session has learned anything"))
    }

    // MARK: Loop

    func testADirectAnswerNeedsOneCall() async throws {
        let model = ScriptedModel([.success(ChatReply(text: " Ana is your manager. "))])
        let earlier = [AskTurn(id: 1, question: "Hello", answer: "Hi."), AskTurn(id: 2, question: "Lost", failure: "offline")]
        let answer = try await agent(try vault(notes), model).answer(question: "Who is my manager?", earlier: earlier, now: now)
        XCTAssertEqual(answer, AskAnswer(text: "Ana is your manager.", sources: [], runningLow: false))
        let request = try XCTUnwrap(model.requests.first)
        XCTAssertEqual(request.model, "model/default")
        XCTAssertEqual(request.messages.map { $0.object["role"] as? String }, ["system", "user", "assistant", "user"])
        XCTAssertEqual(request.messages.last?.object["content"] as? String, "Who is my manager?")
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: request.body) as? [String: Any])
        XCTAssertEqual((body["tools"] as? [[String: Any]])?.count, 4)
        XCTAssertNil(body["tool_choice"])
    }

    func testSearchThenReadThenAnswerRecordsWhatWasRead() async throws {
        let session = try vault(notes)
        let raw = Data(#"{"choices":[{"message":{"role":"assistant","content":null,"reasoning_details":[{"signature":"s1"}],"tool_calls":[{"id":"a","type":"function","function":{"name":"search","arguments":"{\"query\":\"Maestro fix\"}"}}]}}]}"#.utf8)
        let model = ScriptedModel([
            .success(try XCTUnwrap(ChatReply(completion: raw))),
            .success(ChatReply(text: "", toolCalls: [
                call("b", "read", ["path": "daily/2026-09-12.md"]),
                call("c", "read", ["path": "daily/2026-09-12.md"]),
                call("d", "read", ["path": "daily/missing.md"]),
            ])),
            .success(ChatReply(text: "On 12 September (`daily/2026-09-12.md`).")),
        ])
        let seen = LockedLines()
        let answer = try await agent(session, model, grant: grant(used: 85)).answer(question: "When did I deploy the Maestro fix?", now: now) { seen.add($0) }
        XCTAssertEqual(answer.text, "On 12 September (`daily/2026-09-12.md`).")
        XCTAssertEqual(answer.sources, ["daily/2026-09-12.md"])
        XCTAssertTrue(answer.runningLow)
        XCTAssertEqual(seen.lines, ["Looking through your notes for \u{201C}Maestro fix\u{201D}", "Reading daily/2026-09-12.md", "Reading daily/2026-09-12.md", "Reading daily/missing.md"])

        let last = try XCTUnwrap(model.requests.last).messages
        XCTAssertEqual(last.map { $0.object["role"] as? String }, ["system", "user", "assistant", "tool", "assistant", "tool", "tool", "tool"])
        // The model's own message goes back exactly as it came.
        XCTAssertNotNil(last[2].object["reasoning_details"])
        XCTAssertEqual(last[3].object["tool_call_id"] as? String, "a")
        XCTAssertTrue((last[3].object["content"] as? String ?? "").hasPrefix("daily/2026-09-12.md: "))
        XCTAssertTrue((last[5].object["content"] as? String ?? "").contains("Deployed the Maestro fix"))
    }

    func testALoopingModelIsMadeToAnswer() async throws {
        let looping = ChatReply(text: "", toolCalls: [call("x", "list", [:])])
        let model = ScriptedModel(Array(repeating: .success(looping), count: 11) + [.success(ChatReply(text: "Here is what I found."))])
        let answer = try await agent(try vault(notes), model).answer(question: "Everything?", now: now)
        XCTAssertEqual(answer.text, "Here is what I found.")
        XCTAssertEqual(model.requests.count, 12)
        XCTAssertTrue(model.requests[10].allowTools)
        XCTAssertFalse(model.requests[11].allowTools)
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: model.requests[11].body) as? [String: Any])
        XCTAssertEqual(body["tool_choice"] as? String, "none")
    }

    func testFailuresAreSentences() async throws {
        let session = try vault(notes)
        func failure(_ replies: [Result<ChatReply, Error>], grant: AgentGrant? = nil) async -> AskFailure? {
            do {
                _ = try await agent(session, ScriptedModel(replies), grant: grant).answer(question: "?", now: now)
                return nil
            } catch {
                return error as? AskFailure
            }
        }
        let exhausted = ScriptedModel([])
        do {
            _ = try await agent(session, exhausted, grant: grant(status: "exhausted")).answer(question: "?", now: now)
            XCTFail("an exhausted grant must not reach the model")
        } catch {
            XCTAssertEqual(error as? AskFailure, .exhausted)
        }
        XCTAssertTrue(exhausted.requests.isEmpty)

        let offline = await failure([.failure(URLError(.notConnectedToInternet))])
        XCTAssertEqual(offline, .offline)
        let spent = await failure([.failure(GatewayError(status: 402))])
        XCTAssertEqual(spent, .exhausted)
        let busy = await failure([.failure(GatewayError(status: 503))])
        XCTAssertEqual(busy, .busy)
        let silent = await failure([.success(ChatReply(text: "  "))])
        XCTAssertEqual(silent, .noAnswer)

        let lapsed = AskAgent(session: session, transport: ScriptedModel([]), grant: { throw APIError(status: 403, code: "plus_lapsed", error: "Your Thock Plus subscription ended.") })
        do {
            _ = try await lapsed.answer(question: "?", now: now)
            XCTFail("a refused grant must fail the turn")
        } catch {
            XCTAssertEqual((error as? AskFailure)?.sentence, "Your Thock Plus subscription ended.")
        }
    }

    func testACompletionWithStructuredArgumentsStillParses() throws {
        let raw = Data(#"{"choices":[{"message":{"role":"assistant","content":[{"type":"text","text":"Hi"}],"tool_calls":[{"type":"function","function":{"name":"list","arguments":{"folder":"daily"}}}]}}]}"#.utf8)
        let reply = try XCTUnwrap(ChatReply(completion: raw))
        XCTAssertEqual(reply.text, "Hi")
        XCTAssertEqual(reply.toolCalls, [ToolCall(id: "call_0", name: "list", arguments: #"{"folder":"daily"}"#)])
        XCTAssertNil(ChatReply(completion: Data(#"{"error":{"message":"upstream"}}"#.utf8)))
    }

    // MARK: Thread and keeping

    func testTheThreadLastsADay() throws {
        let store = try vault(notes).store
        let today = VaultDay(year: 2026, month: 10, day: 3)
        var turn = try store.addAskTurn(question: "When?", day: today)
        XCTAssertEqual(store.askTurns(day: today), [AskTurn(id: turn.id, question: "When?")])
        turn.answer = "On 12 September."
        turn.sources = ["daily/2026-09-12.md", "projects/maestro.md"]
        turn.kept = true
        try store.finishAskTurn(turn)
        XCTAssertEqual(store.askTurns(day: today), [turn])
        let failed = try store.addAskTurn(question: "And?", day: today)
        try store.removeAskTurn(id: failed.id)
        XCTAssertEqual(store.askTurns(day: today).count, 1)
        XCTAssertEqual(store.askTurns(day: today.adding(days: 1)), [])
        XCTAssertEqual(store.askTurns(day: today), [])
    }

    func testKeepingAnAnswerAppendsItUnderTheAgentsHeading() throws {
        let session = try vault(["daily/2026-10-03.md": "# Saturday\n\n## Journal\n\nA line.\n"])
        XCTAssertTrue(try session.keep(question: "When did I deploy\nthe Maestro fix?", answer: "On 12 September.\n\n## Detail\nRui reviewed it.\n", now: now))
        XCTAssertEqual(session.store.content("daily/2026-10-03.md"), """
            # Saturday

            ## Journal

            A line.

            # Asked on the go

            **14:02** · When did I deploy the Maestro fix?

            On 12 September.

            **Detail**
            Rui reviewed it.

            """)
        XCTAssertFalse(try session.keep(question: "?", answer: "  ", now: now))
        XCTAssertTrue(try session.keep(question: "And who reviewed it?", answer: "Rui.", now: now))
        XCTAssertTrue(session.store.content("daily/2026-10-03.md")?.hasSuffix("Rui reviewed it.\n\n**14:02** · And who reviewed it?\n\nRui.\n") ?? false)
    }
}

final class LockedLines: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: [String] = []

    func add(_ line: String) {
        lock.withLock { stored.append(line) }
    }

    var lines: [String] {
        lock.withLock { stored }
    }
}
