import Foundation

/// One message of a model conversation, kept as the JSON object the gateway
/// speaks. The model's own replies are stored exactly as they arrived, so
/// provider fields this app does not know about (reasoning signatures, for
/// one) go back untouched on the next call.
public struct ChatMessage: Equatable, Sendable {
    public var json: Data

    init(_ object: [String: Any]) {
        json = (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])) ?? Data("{}".utf8)
    }

    public static func system(_ text: String) -> ChatMessage {
        ChatMessage(["role": "system", "content": text])
    }

    public static func user(_ text: String) -> ChatMessage {
        ChatMessage(["role": "user", "content": text])
    }

    public static func assistant(_ text: String) -> ChatMessage {
        ChatMessage(["role": "assistant", "content": text])
    }

    public static func tool(callID: String, name: String, result: String) -> ChatMessage {
        ChatMessage(["role": "tool", "tool_call_id": callID, "name": name, "content": result])
    }

    var object: [String: Any] {
        (try? JSONSerialization.jsonObject(with: json)) as? [String: Any] ?? [:]
    }
}

public struct ToolCall: Equatable, Sendable {
    public var id: String
    public var name: String
    /// The arguments as the JSON text the model wrote.
    public var arguments: String

    public init(id: String, name: String, arguments: String) {
        self.id = id
        self.name = name
        self.arguments = arguments
    }
}

public struct ChatReply: Equatable, Sendable {
    public var message: ChatMessage
    public var text: String
    public var toolCalls: [ToolCall]

    public init(text: String, toolCalls: [ToolCall] = []) {
        var object: [String: Any] = ["role": "assistant", "content": text]
        if !toolCalls.isEmpty {
            object["tool_calls"] = toolCalls.map {
                ["id": $0.id, "type": "function", "function": ["name": $0.name, "arguments": $0.arguments]] as [String: Any]
            }
        }
        self.message = ChatMessage(object)
        self.text = text
        self.toolCalls = toolCalls
    }

    /// Reads `choices[0].message` of a chat completion.
    init?(completion: Data) {
        guard let body = (try? JSONSerialization.jsonObject(with: completion)) as? [String: Any],
              let choice = (body["choices"] as? [[String: Any]])?.first,
              let message = choice["message"] as? [String: Any]
        else { return nil }
        self.message = ChatMessage(message)
        if let content = message["content"] as? String {
            text = content
        } else if let parts = message["content"] as? [[String: Any]] {
            text = parts.compactMap { $0["text"] as? String }.joined()
        } else {
            text = ""
        }
        toolCalls = (message["tool_calls"] as? [[String: Any]] ?? []).enumerated().compactMap { index, call in
            guard let function = call["function"] as? [String: Any], let name = function["name"] as? String else { return nil }
            let arguments: String
            if let written = function["arguments"] as? String {
                arguments = written.isEmpty ? "{}" : written
            } else if let object = function["arguments"], let data = try? JSONSerialization.data(withJSONObject: object) {
                arguments = String(decoding: data, as: UTF8.self)
            } else {
                arguments = "{}"
            }
            return ToolCall(id: call["id"] as? String ?? "call_\(index)", name: name, arguments: arguments)
        }
    }
}

public struct ChatRequest: Sendable {
    public var baseURL: String
    public var apiKey: String
    public var model: String
    public var messages: [ChatMessage]
    /// False on a turn's last call: the model must answer with what it has.
    public var allowTools: Bool

    var body: Data {
        var object: [String: Any] = [
            "model": model,
            "messages": messages.map(\.object),
            "tools": AskTools.definitions,
        ]
        if !allowTools {
            object["tool_choice"] = "none"
        }
        return (try? JSONSerialization.data(withJSONObject: object)) ?? Data()
    }
}

/// The gateway refused or failed a model call.
public struct GatewayError: Error, Equatable, Sendable {
    public var status: Int

    public init(status: Int) {
        self.status = status
    }
}

/// How the agent reaches a model. `GatewayChatTransport` calls the gateway
/// the grant names; tests script the replies.
public protocol ChatTransport: Sendable {
    func complete(_ request: ChatRequest) async throws -> ChatReply
}

public struct GatewayChatTransport: ChatTransport {
    private let session: URLSession

    public init(session: URLSession = .shared) {
        self.session = session
    }

    public func complete(_ request: ChatRequest) async throws -> ChatReply {
        guard let url = URL(string: request.baseURL.hasSuffix("/") ? request.baseURL + "chat/completions" : request.baseURL + "/chat/completions") else {
            throw URLError(.badURL)
        }
        var call = URLRequest(url: url)
        call.httpMethod = "POST"
        call.timeoutInterval = 90
        call.setValue("Bearer \(request.apiKey)", forHTTPHeaderField: "Authorization")
        call.setValue("application/json", forHTTPHeaderField: "Content-Type")
        call.setValue("Thock", forHTTPHeaderField: "X-Title")
        call.httpBody = request.body
        let (data, response) = try await session.data(for: call)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(status) else { throw GatewayError(status: status) }
        guard let reply = ChatReply(completion: data) else {
            // The gateway reports some upstream failures inside a 200.
            throw GatewayError(status: 502)
        }
        return reply
    }
}

public struct AskAnswer: Equatable, Sendable {
    public var text: String
    public var sources: [String]
    /// Set when the allowance is nearly gone, so the screen can say so.
    public var runningLow: Bool
}

/// Why a turn has no answer. `sentence` is what the person reads.
public enum AskFailure: Error, Equatable, Sendable {
    case exhausted
    case offline
    case busy
    case refused(String)
    case noAnswer

    public var sentence: String {
        switch self {
        case .exhausted: return "You've used this cycle's Thock Plus allowance, so the agent is resting until it refills."
        case .offline: return "The agent needs a connection, and there isn't one right now."
        case .busy: return "The agent couldn't be reached just now. Try again in a moment."
        case .refused(let sentence): return sentence
        case .noAnswer: return "The agent looked but didn't come back with an answer. Try asking another way."
        }
    }

    init(_ error: Error) {
        switch error {
        case let failure as AskFailure:
            self = failure
        case let api as APIError:
            self = .refused(api.error)
        case let gateway as GatewayError:
            // A key that ran out of budget is disabled at the gateway.
            self = [401, 402, 403].contains(gateway.status) ? .exhausted : .busy
        case let url as URLError where [.notConnectedToInternet, .networkConnectionLost, .dataNotAllowed, .internationalRoamingOff].contains(url.code):
            self = .offline
        default:
            self = .busy
        }
    }
}

/// The agent's loop on the phone (V35 §5.2): ask the model, run the tools it
/// calls against the local vault, repeat until it answers.
public struct AskAgent: Sendable {
    static let maxModelCalls = 12
    static let earlierTurns = 6

    public let session: VaultSession
    public let transport: ChatTransport
    public let grant: @Sendable () async throws -> AgentGrant

    public init(session: VaultSession, transport: ChatTransport = GatewayChatTransport(), grant: @escaping @Sendable () async throws -> AgentGrant) {
        self.session = session
        self.transport = transport
        self.grant = grant
    }

    /// Answers one question. `earlier` is the day's thread so far; `activity`
    /// hears one plain line per step. Throws `AskFailure`.
    public func answer(question: String, earlier: [AskTurn] = [], now: Date = Date(), activity: @escaping @Sendable (String) -> Void = { _ in }) async throws -> AskAnswer {
        do {
            let grant = try await grant()
            guard !grant.isExhausted else { throw AskFailure.exhausted }

            var messages = [ChatMessage.system(AskPrompt.system(session: session, now: now))]
            for turn in earlier.suffix(Self.earlierTurns) {
                guard let answer = turn.answer else { continue }
                messages.append(.user(turn.question))
                messages.append(.assistant(answer))
            }
            messages.append(.user(question))

            let tools = AskTools(session: session, now: now)
            var sources: [String] = []
            for call in 1...Self.maxModelCalls {
                try Task.checkCancellation()
                let reply = try await transport.complete(ChatRequest(
                    baseURL: grant.gateway.baseURL, apiKey: grant.gateway.apiKey, model: grant.gateway.models.default,
                    messages: messages, allowTools: call < Self.maxModelCalls))
                guard !reply.toolCalls.isEmpty, call < Self.maxModelCalls else {
                    let text = reply.text.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !text.isEmpty else { throw AskFailure.noAnswer }
                    return AskAnswer(text: text, sources: sources, runningLow: grant.isRunningLow)
                }
                messages.append(reply.message)
                for toolCall in reply.toolCalls {
                    let outcome = tools.run(name: toolCall.name, arguments: toolCall.arguments)
                    activity(outcome.activity)
                    if let source = outcome.source, !sources.contains(source) {
                        sources.append(source)
                    }
                    messages.append(.tool(callID: toolCall.id, name: toolCall.name, result: outcome.result))
                }
            }
            throw AskFailure.noAnswer
        } catch is CancellationError {
            throw CancellationError()
        } catch let error as URLError where error.code == .cancelled {
            throw CancellationError()
        } catch {
            throw AskFailure(error)
        }
    }
}
