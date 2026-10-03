import XCTest
@testable import ThockKit

/// Serves `URLSession` requests from the in-process server, so the real HTTP
/// transport is exercised end to end: URLs, headers, bodies, status codes and
/// the event stream.
final class LoopbackProtocol: URLProtocol {
    nonisolated(unsafe) static var backend: LocalBackend?
    nonisolated(unsafe) static var seen: [(method: String, url: String, client: String?)] = []
    static let feed = "event: file\ndata: {\"path\":\"daily/2026-10-02.md\",\"version\":9,\"deleted\":false}\n\n: ping\n\nevent: surprise\ndata: {\"x\":1}\n\nevent: ack\ndata: {\"through_seq\":3,\"at_version\":9}\n\n"

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}

    override func startLoading() {
        guard let url = request.url, let components = URLComponents(url: url, resolvingAgainstBaseURL: false), let backend = Self.backend else { return }
        let method = request.httpMethod ?? "GET"
        Self.seen.append((method, url.absoluteString, request.value(forHTTPHeaderField: "Thock-Client")))
        var body: Data?
        if let stream = request.httpBodyStream {
            stream.open()
            var data = Data()
            var buffer = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable {
                let read = stream.read(&buffer, maxLength: buffer.count)
                if read <= 0 { break }
                data.append(buffer, count: read)
            }
            body = data
        } else {
            body = request.httpBody
        }
        let credential = request.value(forHTTPHeaderField: "Authorization").map { String($0.dropFirst("Bearer ".count)) }
        var query: [String: String] = [:]
        for item in components.queryItems ?? [] {
            query[item.name] = item.value
        }
        let client = client
        let requestBody = body
        Task {
            func respond(_ status: Int, _ data: Data, type: String) {
                let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": type])!
                client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
                client?.urlProtocol(self, didLoad: data)
                client?.urlProtocolDidFinishLoading(self)
            }
            if components.path == "/base/v1/vault/feed" {
                respond(200, Data(Self.feed.utf8), type: "text/event-stream")
                return
            }
            if components.path.hasPrefix("/base/blob/") {
                let blob = try? await backend.download("thock-local://blob/" + components.path.dropFirst("/base/blob/".count))
                respond(blob == nil ? 404 : 200, blob ?? Data(), type: "application/octet-stream")
                return
            }
            do {
                let result = try await backend.send(method: method, path: String(components.percentEncodedPath.dropFirst("/base".count)), query: query, body: requestBody, credential: credential)
                var data = result.body
                // The real server signs https URLs; point the local ones at this loopback.
                if let text = String(data: data, encoding: .utf8) {
                    data = Data(text.replacingOccurrences(of: "thock-local:\\/\\/blob\\/", with: "https:\\/\\/plus.test\\/base\\/blob\\/").utf8)
                }
                respond(result.status, data, type: "application/json; charset=utf-8")
            } catch {
                client?.urlProtocol(self, didFailWithError: error)
            }
        }
    }
}

final class HTTPTransportTests: XCTestCase {
    func testPairingAndARoundOverRealHTTPRequests() async throws {
        let backend = LocalBackend()
        let day = VaultDay(year: 2026, month: 10, day: 2)
        let desk = SimulatedDesk(backend: backend, state: .init(disk: SampleVault.make(today: day).files, key: Data(repeating: 4, count: 32)), today: { day })
        try await desk.open()
        LoopbackProtocol.backend = backend
        LoopbackProtocol.seen = []
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [LoopbackProtocol.self]
        let transport = HTTPTransport(base: URL(string: "https://plus.test/base")!, appVersion: "0.1", session: URLSession(configuration: configuration))

        let store = try VaultStore(url: nil)
        let engine = SyncEngine(store: store, transport: transport, secrets: MemorySecretStore())
        var link = try await desk.pairingLink()
        link.backend = "https://plus.test/base"
        try await engine.pair(link: link, deviceName: "Test iPhone")

        let disk = await desk.state.disk
        XCTAssertEqual(store.content("daily/2026-10-02.md"), disk["daily/2026-10-02.md"])
        XCTAssertEqual(Set(store.paths()), Set(disk.keys))

        let session = VaultSession(store: store)
        try session.capture(blocks: Blocks.parse("Over the wire"), destination: .backlog)
        await engine.sync()
        for _ in 0..<50 where await backend.state.writes.count > 0 {
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        await engine.sync()
        XCTAssertEqual(store.waitingForDeskCount, 0)
        let backlog = await desk.file("backlog.md")
        XCTAssertTrue(backlog?.contains("- [ ] Over the wire") ?? false)

        let seen = LoopbackProtocol.seen
        XCTAssertEqual(seen.first?.method, "POST")
        XCTAssertEqual(seen.first?.url, "https://plus.test/base/v1/vault/pair")
        XCTAssertTrue(seen.allSatisfy { $0.client == "phone/0.1" || $0.url.contains("/blob/") })
        XCTAssertTrue(seen.contains { $0.url == "https://plus.test/base/v1/vault/files?limit=500" })
        XCTAssertTrue(seen.contains { $0.url.hasPrefix("https://plus.test/base/v1/vault/files?limit=500&since=") })
        XCTAssertTrue(seen.contains { $0.method == "POST" && $0.url == "https://plus.test/base/v1/vault/writes" })

        var events: [FeedEvent] = []
        for try await event in transport.feed(credential: "tpp_x") {
            events.append(event)
        }
        XCTAssertEqual(events, [.file(path: "daily/2026-10-02.md", version: 9, deleted: false), .ack(throughSeq: 3, atVersion: 9)])
    }

    func testAnErrorBodyBecomesItsCode() async throws {
        let backend = LocalBackend()
        LoopbackProtocol.backend = backend
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [LoopbackProtocol.self]
        let transport = HTTPTransport(base: URL(string: "https://plus.test/base")!, session: URLSession(configuration: configuration))
        let engine = SyncEngine(store: try VaultStore(url: nil), transport: transport, secrets: MemorySecretStore())
        do {
            try await engine.pair(link: PairingLink(code: "AAAA-AAAA", key: Data(repeating: 1, count: 32), backend: "https://plus.test/base"), deviceName: "x")
            XCTFail("an unknown code must not pair")
        } catch {
            XCTAssertEqual(error as? PairingError, .refused("That code didn't work. Try again."))
        }
    }
}
