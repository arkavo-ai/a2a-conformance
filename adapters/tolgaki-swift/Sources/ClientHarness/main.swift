// ClientHarness — wraps tolgaki's a2a-swift client SDK (third party, under test).
//
// Upstream: https://github.com/tolgaki/a2a-swift @ 5b0afd9299d2fd6eca2bd93a07140bdd6c0cdabc
// Module aliased to TolgakiA2AClient (name collides with the server SDK's
// a2a-client-swift target); the SDK itself is untouched.
//
// Contract: reads NDJSON op lines on stdin, performs each op through the
// SDK's native API, emits one NDJSON outcome line on stdout. Logs → stderr.
//
// Fairness rules honored here:
//  - `value` is always the SDK's decoded result re-encoded with the SDK's
//    own Codable encoders (JSONEncoder + .iso8601, matching the SDK's
//    transport encoder). Wire bytes are never echoed back.
//  - For SendMessage the SDK's result type is the enum
//    SendMessageResponse(.task/.message); the harness reports which case the
//    SDK decoded by wrapping the SDK-encoded payload in the v1.0 oneof key
//    ({"task": ...}/{"message": ...}). The payload itself is untouched SDK
//    output. (The SDK's own `encode(to:)` for this enum is flat, which would
//    erase which case was decoded — the wrap reports the SDK's decode
//    decision, it does not amend the SDK's field encoding.)
//  - A2AError.invalidRequest is reported with errorCode null: the SDK maps
//    BOTH -32600 and -32602 wire codes onto that one case, so any specific
//    number here would be fabricated.

import Foundation
// The product is module-aliased in Package.swift to avoid colliding with the
// server graph's a2a-client-swift target; within this target the module is
// still referred to by its original name.
import A2AClient // tolgaki/a2a-swift; @_exported imports A2ACore (Message, A2ATask, ...)
import HarnessSupport

// MARK: - SDK codec helpers (match the SDK transport's encoder/decoder config)

func sdkEncoder() -> JSONEncoder {
    let e = JSONEncoder()
    e.dateEncodingStrategy = .iso8601
    return e
}

func sdkDecoder() -> JSONDecoder {
    let d = JSONDecoder()
    d.dateDecodingStrategy = .iso8601
    return d
}

func encodeSDK<T: Encodable>(_ value: T) throws -> JSONValue {
    try JSONValue.parse(sdkEncoder().encode(value))
}

func decodeSDK<T: Decodable>(_ type: T.Type, from json: JSONValue) throws -> T {
    try sdkDecoder().decode(type, from: json.serialized(sortedKeys: false))
}

// MARK: - Error mapping

/// Extracts the protocol error code the SDK surfaced, if the SDK retained one.
func protocolErrorCode(_ error: A2AError) -> Int? {
    switch error {
    case .taskNotFound: return JSONRPCErrorCode.taskNotFound.rawValue                              // -32001
    case .taskNotCancelable: return JSONRPCErrorCode.taskNotCancelable.rawValue                    // -32002
    case .pushNotificationNotSupported: return JSONRPCErrorCode.pushNotificationNotSupported.rawValue // -32003
    case .unsupportedOperation: return JSONRPCErrorCode.unsupportedOperation.rawValue              // -32004
    case .contentTypeNotSupported: return JSONRPCErrorCode.contentTypeNotSupported.rawValue        // -32005
    case .invalidAgentResponse: return JSONRPCErrorCode.invalidAgentResponse.rawValue              // -32006
    case .extendedAgentCardNotConfigured: return JSONRPCErrorCode.extendedAgentCardNotConfigured.rawValue // -32007
    case .extensionSupportRequired: return JSONRPCErrorCode.extensionSupportRequired.rawValue      // -32008
    case .versionNotSupported: return JSONRPCErrorCode.versionNotSupported.rawValue                // -32009
    case .authenticationRequired: return JSONRPCErrorCode.authenticationRequired.rawValue          // -32010 (SDK extension)
    case .authorizationFailed: return JSONRPCErrorCode.authorizationFailed.rawValue                // -32011 (SDK extension)
    case .internalError: return JSONRPCErrorCode.internalError.rawValue                            // -32603
    case .jsonRPCError(let code, _, _): return code
    case .invalidRequest:
        // SDK collapses -32600 and -32602 into one case; the original code is
        // unrecoverable from the surfaced error. Reporting either number
        // would be a guess.
        return nil
    case .networkError, .encodingError, .invalidResponse, .unknown:
        return nil
    }
}

func errorOutcome(_ error: Error, detail: String? = nil) -> JSONValue {
    var o: [String: JSONValue] = ["kind": .string("error")]
    if let a2a = error as? A2AError {
        if let code = protocolErrorCode(a2a) {
            o["errorCode"] = .int(code)
        } else {
            o["errorCode"] = .null
        }
        o["errorMessage"] = .string(a2a.errorDescription ?? String(describing: a2a))
    } else {
        o["errorCode"] = .null
        o["errorMessage"] = .string(String(describing: error))
    }
    if let detail { o["detail"] = .string(detail) }
    return .object(o)
}

// MARK: - Timeout

struct HarnessTimeout: Error {}

func withTimeout<T: Sendable>(
    milliseconds: Int,
    _ body: @escaping @Sendable () async throws -> T
) async throws -> T {
    try await withThrowingTaskGroup(of: T.self) { group in
        group.addTask { try await body() }
        group.addTask {
            try await Task.sleep(nanoseconds: UInt64(max(milliseconds, 1)) * 1_000_000)
            throw HarnessTimeout()
        }
        guard let result = try await group.next() else { throw HarnessTimeout() }
        group.cancelAll()
        return result
    }
}

// MARK: - Client construction

func makeClient(baseUrl: String, bearer: String? = nil) throws -> A2AClient {
    guard let url = URL(string: baseUrl) else {
        throw A2AError.invalidRequest(message: "harness: invalid baseUrl \(baseUrl)")
    }
    // The SDK's configuration default is .httpREST; the conformance matrix
    // exercises the JSON-RPC binding, so it is selected explicitly.
    var config = A2AClientConfiguration(baseURL: url, transportBinding: .jsonRPC)
    if let bearer { config = config.withBearerToken(bearer) }
    return A2AClient(configuration: config)
}

func wellKnownCardURL(_ baseUrl: String) throws -> URL {
    guard let url = URL(string: baseUrl.hasSuffix("/")
        ? baseUrl + ".well-known/agent-card.json"
        : baseUrl + "/.well-known/agent-card.json") else {
        throw A2AError.invalidRequest(message: "harness: invalid baseUrl \(baseUrl)")
    }
    return url
}

/// For discovery/* RPC scenarios the client is built from the served agent
/// card via the SDK's own `init(agentCard:)`, so the SDK's interface
/// selection (and tenant propagation) logic is what actually runs.
func makeClientFromCard(baseUrl: String) async throws -> A2AClient {
    let card = try await A2AClient.fetchAgentCard(from: wellKnownCardURL(baseUrl))
    return try A2AClient(agentCard: card)
}

// MARK: - Streaming collection

func collectStream(_ stream: AsyncThrowingStream<StreamingEvent, Error>) async throws -> JSONValue {
    var events: [JSONValue] = []
    do {
        for try await event in stream {
            let (kind, value): (String, JSONValue)
            switch event {
            case .task(let task): (kind, value) = ("task", try encodeSDK(task))
            case .message(let message): (kind, value) = ("message", try encodeSDK(message))
            case .taskStatusUpdate(let e): (kind, value) = ("statusUpdate", try encodeSDK(e))
            case .taskArtifactUpdate(let e): (kind, value) = ("artifactUpdate", try encodeSDK(e))
            }
            events.append(.object(["kind": .string(kind), "value": value]))
        }
    } catch {
        if error is HarnessTimeout || error is CancellationError { throw error }
        return errorOutcome(error, detail: "stream failed after \(events.count) event(s)")
    }
    return .object(["kind": .string("stream"), "events": .array(events)])
}

// MARK: - Op dispatch

func performOp(op: String, scenario: String, baseUrl: String, params: JSONValue?, rawBody: String?) async throws -> JSONValue {
    switch op {
    case "SendMessage":
        guard let messageJSON = params?["message"] else {
            return .object(["kind": .string("harness-error"),
                            "detail": .string("SendMessage params missing message")])
        }
        let message = try decodeSDK(Message.self, from: messageJSON)
        let configuration = try params?["configuration"].map { try decodeSDK(MessageSendConfiguration.self, from: $0) }
        let client: A2AClient
        if scenario.hasPrefix("discovery/") {
            client = try await makeClientFromCard(baseUrl: baseUrl)
        } else {
            client = try makeClient(baseUrl: baseUrl)
        }
        let response = try await client.sendMessage(message, configuration: configuration)
        // Report which case the SDK decoded via the v1.0 oneof key; the
        // payload is the SDK's own encoding (see header comment).
        switch response {
        case .task(let task):
            return .object(["kind": .string("result"), "value": .object(["task": try encodeSDK(task)])])
        case .message(let msg):
            return .object(["kind": .string("result"), "value": .object(["message": try encodeSDK(msg)])])
        }

    case "SendStreamingMessage":
        guard let messageJSON = params?["message"] else {
            return .object(["kind": .string("harness-error"),
                            "detail": .string("SendStreamingMessage params missing message")])
        }
        let message = try decodeSDK(Message.self, from: messageJSON)
        let configuration = try params?["configuration"].map { try decodeSDK(MessageSendConfiguration.self, from: $0) }
        let client = scenario.hasPrefix("discovery/")
            ? try await makeClientFromCard(baseUrl: baseUrl)
            : try makeClient(baseUrl: baseUrl)
        let stream = try await client.sendStreamingMessage(message, configuration: configuration)
        return try await collectStream(stream)

    case "GetTask":
        guard let id = params?["id"]?.stringValue else {
            return .object(["kind": .string("harness-error"), "detail": .string("GetTask params missing id")])
        }
        let historyLength = params?["historyLength"]?.intValue
        let task = try await makeClient(baseUrl: baseUrl).getTask(id, historyLength: historyLength)
        return .object(["kind": .string("result"), "value": try encodeSDK(task)])

    case "ListTasks":
        let query = try params.map { try decodeSDK(TaskQueryParams.self, from: $0) } ?? TaskQueryParams()
        let list = try await makeClient(baseUrl: baseUrl).listTasks(query)
        return .object(["kind": .string("result"), "value": try encodeSDK(list)])

    case "CancelTask":
        guard let id = params?["id"]?.stringValue else {
            return .object(["kind": .string("harness-error"), "detail": .string("CancelTask params missing id")])
        }
        let metadata = try params?["metadata"].map { try decodeSDK([String: AnyCodable].self, from: $0) }
        let task = try await makeClient(baseUrl: baseUrl).cancelTask(id, metadata: metadata)
        return .object(["kind": .string("result"), "value": try encodeSDK(task)])

    case "SubscribeToTask":
        guard let id = params?["id"]?.stringValue else {
            return .object(["kind": .string("harness-error"), "detail": .string("SubscribeToTask params missing id")])
        }
        let stream = try await makeClient(baseUrl: baseUrl).subscribeToTask(id)
        return try await collectStream(stream)

    case "CreateTaskPushNotificationConfig":
        guard let taskId = params?["taskId"]?.stringValue else {
            return .object(["kind": .string("harness-error"),
                            "detail": .string("CreateTaskPushNotificationConfig params missing taskId")])
        }
        let config: PushNotificationConfig
        if let configJSON = params?["config"] {
            config = try decodeSDK(PushNotificationConfig.self, from: configJSON)
        } else if let url = params?["url"]?.stringValue {
            config = PushNotificationConfig(url: url)
        } else {
            return .object(["kind": .string("harness-error"),
                            "detail": .string("CreateTaskPushNotificationConfig params missing config/url")])
        }
        let result = try await makeClient(baseUrl: baseUrl)
            .createPushNotificationConfig(taskId: taskId, config: config)
        return .object(["kind": .string("result"), "value": try encodeSDK(result)])

    case "GetExtendedAgentCard":
        // The SDK documents this op as requiring authentication; the harness
        // supplies a static bearer token (the conformance corpus never
        // checks credential contents). Without it, tolgaki's server gates the
        // op behind its own -32010 authenticationRequired before consulting
        // the handler — sending the token lets the op under test execute.
        let card = try await makeClient(baseUrl: baseUrl, bearer: "conformance-harness").getExtendedAgentCard()
        return .object(["kind": .string("result"), "value": try encodeSDK(card)])

    case "ResolveCard":
        let card = try await A2AClient.fetchAgentCard(from: wellKnownCardURL(baseUrl))
        return .object(["kind": .string("card"), "value": try encodeSDK(card)])

    case "SelectInterface":
        let card = try await A2AClient.fetchAgentCard(from: wellKnownCardURL(baseUrl))
        let config = try A2AClientConfiguration.from(agentCard: card)
        // The SDK does not retain the chosen AgentInterface object; the
        // publicly readable configuration fields it derived from it are
        // reported back in AgentInterface shape.
        var iface: [String: JSONValue] = [
            "url": .string(config.baseURL.absoluteString),
            "protocolBinding": .string(config.transportBinding.rawValue),
            "protocolVersion": .string(config.protocolVersion),
        ]
        if let tenant = config.tenant { iface["tenant"] = .string(tenant) }
        return .object(["kind": .string("interface"), "value": .object(iface)])

    case "RawRequest":
        // Deliberately bypasses the SDK per contract.
        guard let rawBody else {
            return .object(["kind": .string("harness-error"), "detail": .string("RawRequest missing rawBody")])
        }
        guard let url = URL(string: baseUrl) else {
            return .object(["kind": .string("harness-error"), "detail": .string("invalid baseUrl")])
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = Data(rawBody.utf8)
        let (data, _) = try await URLSession.shared.data(for: request)
        let envelope = try JSONValue.parse(data)
        if let err = envelope["error"], let code = err["code"]?.intValue {
            return .object([
                "kind": .string("error"),
                "errorCode": .int(code),
                "errorMessage": .string(err["message"]?.stringValue ?? ""),
            ])
        }
        if let result = envelope["result"] {
            return .object(["kind": .string("result"), "value": result])
        }
        return .object([
            "kind": .string("error"),
            "errorCode": .null,
            "errorMessage": .string("response had neither result nor error: \(String(decoding: data.prefix(200), as: UTF8.self))"),
        ])

    default:
        return .object([
            "kind": .string("unsupported"),
            "detail": .string("op \(op) is not implemented by this harness"),
        ])
    }
}

// MARK: - Main loop

let implVersion = "5b0afd9" // pinned a2a-swift SHA prefix
let implObject: JSONValue = .object(["name": .string("tolgaki-swift"), "version": .string(implVersion)])

HarnessIO.log("tolgaki-swift client harness starting (SDK a2a-swift @ \(implVersion))")

while let line = readLine(strippingNewline: true) {
    let trimmed = line.trimmingCharacters(in: .whitespaces)
    if trimmed.isEmpty { continue }

    var scenario = "unknown"
    let start = Date()
    var outcome: JSONValue

    do {
        let input = try JSONValue.parse(trimmed)
        scenario = input["scenario"]?.stringValue ?? "unknown"
        let baseUrl = input["baseUrl"]?.stringValue ?? ""
        let op = input["op"]?.stringValue ?? ""
        let params = input["params"].flatMap { $0.isNull ? nil : $0 }
        let rawBody = input["rawBody"]?.stringValue
        let timeoutMs = input["timeoutMs"]?.intValue ?? 30000

        HarnessIO.log("scenario=\(scenario) op=\(op) baseUrl=\(baseUrl)")

        let scenarioID = scenario
        do {
            outcome = try await withTimeout(milliseconds: timeoutMs) {
                try await performOp(op: op, scenario: scenarioID, baseUrl: baseUrl, params: params, rawBody: rawBody)
            }
        } catch is HarnessTimeout {
            outcome = .object([
                "kind": .string("harness-error"),
                "detail": .string("op timed out after \(timeoutMs) ms"),
            ])
        } catch let error as DecodingError {
            // The harness failed to decode scenario params into SDK types —
            // i.e. the SDK's request types cannot represent these params.
            outcome = .object([
                "kind": .string("unsupported"),
                "detail": .string("SDK request types cannot represent scenario params: \(error)"),
            ])
        } catch {
            outcome = errorOutcome(error)
        }
    } catch {
        outcome = .object([
            "kind": .string("harness-error"),
            "detail": .string("failed to parse input line: \(error)"),
        ])
    }

    let durationMs = Date().timeIntervalSince(start) * 1000
    let lineOut: JSONValue = .object([
        "scenario": .string(scenario),
        "outcome": outcome,
        "durationMs": .double((durationMs * 100).rounded() / 100),
        "impl": implObject,
    ])
    do {
        HarnessIO.emitLine(try lineOut.serializedString())
    } catch {
        HarnessIO.emitLine(#"{"scenario":"\#(scenario)","outcome":{"kind":"harness-error","detail":"outcome serialization failed"},"durationMs":0,"impl":{"name":"tolgaki-swift","version":"\#(implVersion)"}}"#)
    }
}

HarnessIO.log("stdin EOF — client harness exiting")
