import Foundation

/// The Plus backend over HTTPS (V34 API §3.1).
public final class HTTPTransport: SyncTransport, @unchecked Sendable {
    private let base: URL
    private let session: URLSession
    private let client: String

    public init(base: URL, appVersion: String = "0.1", session: URLSession = .shared) {
        self.base = base
        self.session = session
        self.client = "phone/\(appVersion)"
    }

    private func url(_ path: String, query: [String: String]) -> URL {
        // Paths arrive with their segments already percent-encoded.
        var components = URLComponents(url: base, resolvingAgainstBaseURL: false) ?? URLComponents()
        let prefix = components.percentEncodedPath.hasSuffix("/") ? String(components.percentEncodedPath.dropLast()) : components.percentEncodedPath
        components.percentEncodedPath = prefix + path
        if !query.isEmpty {
            components.queryItems = query.sorted { $0.key < $1.key }.map { URLQueryItem(name: $0.key, value: $0.value) }
        }
        return components.url ?? base
    }

    public func send(method: String, path: String, query: [String: String], body: Data?, credential: String?) async throws -> HTTPResult {
        var request = URLRequest(url: url(path, query: query))
        request.httpMethod = method
        request.setValue(client, forHTTPHeaderField: "Thock-Client")
        if let credential {
            request.setValue("Bearer \(credential)", forHTTPHeaderField: "Authorization")
        }
        if let body {
            request.httpBody = body
            request.setValue("application/json; charset=utf-8", forHTTPHeaderField: "Content-Type")
        }
        let (data, response) = try await session.data(for: request)
        return HTTPResult(status: (response as? HTTPURLResponse)?.statusCode ?? 0, body: data)
    }

    public func download(_ url: String) async throws -> Data {
        guard let target = URL(string: url, relativeTo: base) else { throw URLError(.badURL) }
        let (data, response) = try await session.data(from: target)
        guard let status = (response as? HTTPURLResponse)?.statusCode, (200..<300).contains(status) else {
            throw URLError(.badServerResponse)
        }
        return data
    }

    public func upload(_ url: String, body: Data, headers: [String: String]) async throws {
        guard let target = URL(string: url, relativeTo: base) else { throw URLError(.badURL) }
        var request = URLRequest(url: target)
        request.httpMethod = "PUT"
        for (name, value) in headers {
            request.setValue(value, forHTTPHeaderField: name)
        }
        let (_, response) = try await session.upload(for: request, from: body)
        guard let status = (response as? HTTPURLResponse)?.statusCode, (200..<300).contains(status) else {
            throw URLError(.badServerResponse)
        }
    }

    public func feed(credential: String) -> AsyncThrowingStream<FeedEvent, Error> {
        var request = URLRequest(url: url("/v1/vault/feed", query: [:]))
        request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        request.setValue("Bearer \(credential)", forHTTPHeaderField: "Authorization")
        request.setValue(client, forHTTPHeaderField: "Thock-Client")
        request.timeoutInterval = 3600
        let session = session
        return AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    let (bytes, response) = try await session.bytes(for: request)
                    guard let status = (response as? HTTPURLResponse)?.statusCode, status == 200 else {
                        throw APIError(status: (response as? HTTPURLResponse)?.statusCode ?? 0, code: "unavailable", error: "The feed did not open.")
                    }
                    var event = ""
                    var data = ""
                    for try await line in bytes.lines {
                        // `lines` drops the blank separator, so an event is
                        // complete as soon as its data line has been read.
                        if line.hasPrefix("event:") {
                            event = line.dropFirst(6).trimmingCharacters(in: .whitespaces)
                        } else if line.hasPrefix("data:") {
                            data = line.dropFirst(5).trimmingCharacters(in: .whitespaces)
                            if let parsed = FeedEvent(event: event, data: data) {
                                continuation.yield(parsed)
                            }
                            event = ""
                            data = ""
                        }
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}
