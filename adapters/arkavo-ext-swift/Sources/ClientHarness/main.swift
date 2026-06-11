// arkavo-ext-swift client harness (a2a-conformance adapter contract v1).
//
// The core adapters/arkavo-swift client harness, extended with scenario-keyed
// identity behavior (IDENTITY-HARNESS.md) implemented in IdentityOps.swift:
// fresh-CWT presentation via ArkavoA2AIdentity's CwtAuthenticator, fixed bad
// tokens via a static-header RequestAuthenticator, and fail-closed card
// verification (verifyCard(.requireIdentity)). Every other scenario behaves
// byte-identically to adapters/arkavo-swift.
//
// Reads NDJSON input lines on stdin, performs each op through a2a-swift's
// native client API, and emits one NDJSON outcome line per input on stdout
// (schema/harness-outcome.schema.json). Logs go to stderr. Exits on stdin EOF.

import A2A
import A2AClient
import Foundation

#if canImport(FoundationNetworking)
    import FoundationNetworking
#endif

let implName = "arkavo-ext-swift"
let implVersion = "0.1.0"

func logError(_ message: String) {
    FileHandle.standardError.write(Data("client-harness: \(message)\n".utf8))
}

// MARK: - Input lines

struct InputLine: Sendable {
    var scenario: String
    var baseUrl: String
    var op: String
    /// `params` re-serialized to JSON bytes (decoded into SDK request types).
    var params: Data?
    var rawBody: String?
    var timeoutMs: Int

    init?(parsing line: String) {
        guard
            let object = (try? JSONSerialization.jsonObject(with: Data(line.utf8)))
                as? [String: Any],
            let scenario = object["scenario"] as? String,
            let baseUrl = object["baseUrl"] as? String,
            let op = object["op"] as? String
        else {
            return nil
        }
        self.scenario = scenario
        self.baseUrl = baseUrl
        self.op = op
        if let params = object["params"], !(params is NSNull) {
            self.params = try? JSONSerialization.data(
                withJSONObject: params, options: [.fragmentsAllowed, .sortedKeys])
        } else {
            self.params = nil
        }
        self.rawBody = object["rawBody"] as? String
        self.timeoutMs = (object["timeoutMs"] as? NSNumber)?.intValue ?? 30_000
    }
}

// MARK: - JSON plumbing

func encodeToJSONValue<Value: Encodable>(_ value: Value) throws -> JSONValue {
    let data = try A2AJSON.encoder().encode(value)
    return try A2AJSON.decoder().decode(JSONValue.self, from: data)
}

func decodeParams<Request: Decodable>(_ type: Request.Type, from data: Data?) throws -> Request {
    try A2AJSON.decoder().decode(Request.self, from: data ?? Data("{}".utf8))
}

func resultOutcome(_ value: JSONValue, kind: String = "result") -> JSONValue {
    .object(["kind": .string(kind), "value": value])
}

func errorOutcome(_ error: any Error) -> JSONValue {
    if let a2aError = error as? A2AError {
        return .object([
            "kind": .string("error"),
            "errorCode": .number(Double(a2aError.code)),
            "errorMessage": .string(a2aError.message),
        ])
    }
    // Non-protocol error the SDK surfaced (e.g. a strict-decode failure):
    // kind=error with a null errorCode per the contract.
    return .object([
        "kind": .string("error"),
        "errorCode": .null,
        "errorMessage": .string(String(describing: error)),
    ])
}

func harnessErrorOutcome(_ detail: String) -> JSONValue {
    .object(["kind": .string("harness-error"), "detail": .string(detail)])
}

// MARK: - Client construction

func makeTransport(timeoutMs: Int) -> URLSessionTransport {
    let configuration = URLSessionConfiguration.ephemeral
    let seconds = Double(timeoutMs) / 1000.0
    configuration.timeoutIntervalForRequest = max(seconds, 1)
    configuration.timeoutIntervalForResource = max(seconds, 1)
    return URLSessionTransport(configuration: configuration)
}

func sameAuthority(_ left: String, _ right: String) -> Bool {
    guard let leftURL = URL(string: left), let rightURL = URL(string: right) else {
        return false
    }
    func port(_ url: URL) -> Int {
        url.port ?? (url.scheme?.lowercased() == "https" ? 443 : 80)
    }
    return leftURL.host?.lowercased() == rightURL.host?.lowercased()
        && port(leftURL) == port(rightURL)
}

/// Builds the SDK client for an RPC op.
///
/// Card-first rule: resolve the agent card from `baseUrl` and construct
/// `A2AClient(card:)` whenever resolution succeeds and the selected
/// interface's host:port matches `baseUrl`; otherwise fall back to a synthetic
/// JSONRPC interface pointed at `baseUrl`. This keeps interface metadata the
/// card declares (notably `tenant`, exercised by discovery/tenant-echo) in
/// play exactly when the card actually routes back to the server under test,
/// and stays deterministic: same card + same baseUrl always yields the same
/// client.
func makeClient(baseUrl: String, transport: URLSessionTransport) async -> A2AClient {
    if let cardURL = URL(string: baseUrl + A2AProtocol.agentCardWellKnownPath),
        let card = try? await AgentCardResolver(transport: transport).resolve(url: cardURL),
        let interface = AgentCardResolver.selectInterface(from: card),
        sameAuthority(interface.url, baseUrl),
        let client = try? A2AClient(card: card, transport: transport)
    {
        return client
    }
    let synthetic = AgentInterface(
        url: baseUrl,
        protocolBinding: AgentInterface.Binding.jsonrpc,
        protocolVersion: A2AProtocol.version)
    return A2AClient(interface: synthetic, transport: transport)
}

// MARK: - Ops

func streamOutcome(_ stream: AsyncThrowingStream<StreamResponse, Error>) async -> JSONValue {
    var events: [JSONValue] = []
    do {
        for try await event in stream {
            let item: JSONValue
            switch event {
            case .task(let task):
                item = .object(["kind": .string("task"), "value": try encodeToJSONValue(task)])
            case .message(let message):
                item = .object([
                    "kind": .string("message"), "value": try encodeToJSONValue(message),
                ])
            case .statusUpdate(let update):
                item = .object([
                    "kind": .string("statusUpdate"), "value": try encodeToJSONValue(update),
                ])
            case .artifactUpdate(let update):
                item = .object([
                    "kind": .string("artifactUpdate"), "value": try encodeToJSONValue(update),
                ])
            }
            events.append(item)
        }
    } catch {
        // The call (or the stream itself) failed: report the error. The
        // corpus only scripts pre-event failures, so no events are lost.
        return errorOutcome(error)
    }
    return .object(["kind": .string("stream"), "events": .array(events)])
}

func rawRequestOutcome(_ input: InputLine) async -> JSONValue {
    guard let url = URL(string: input.baseUrl) else {
        return harnessErrorOutcome("invalid baseUrl \(input.baseUrl)")
    }
    guard let rawBody = input.rawBody else {
        return harnessErrorOutcome("RawRequest without rawBody")
    }
    var request = URLRequest(url: url)
    request.httpMethod = "POST"
    request.setValue("application/json", forHTTPHeaderField: "Content-Type")
    request.httpBody = Data(rawBody.utf8)
    do {
        // Plain HTTP on purpose: RawRequest is the only op that bypasses the SDK.
        let (data, _) = try await URLSession.shared.data(for: request)
        let envelope = try A2AJSON.decoder().decode(
            JSONRPCResponse<JSONValue>.self, from: data)
        if let error = envelope.error {
            return .object([
                "kind": .string("error"),
                "errorCode": .number(Double(error.code)),
                "errorMessage": .string(error.message),
            ])
        }
        if let result = envelope.result {
            return resultOutcome(result)
        }
        return harnessErrorOutcome("envelope had neither result nor error")
    } catch {
        return harnessErrorOutcome("raw request failed: \(error)")
    }
}

func performOp(_ input: InputLine) async -> JSONValue {
    // Scenario-keyed identity behavior lives here in the harness, exactly
    // like the tolgaki harness's loopback seeding: SDK code paths never see
    // scenario ids.
    if input.scenario.hasPrefix("arkavo/identity/") {
        return await performIdentityOp(input)
    }
    // TDF parts (tdf-parts-v1, TDF-HARNESS.md): decrypt/verify the encrypted
    // part, surface fail-closed #error metadata. Scenario-keyed in the harness.
    if input.scenario.hasPrefix("arkavo/tdf/") {
        return await performTDFOp(input)
    }
    // WS binding (ws-binding-v1, WS-HARNESS.md): transport selection by
    // scenario. ws/* drives the SDK WS client; transport-equivalence/* runs the
    // op over both HTTP and WS and asserts identical decoded results.
    if input.scenario.hasPrefix("arkavo/ws/") {
        return await performWSOp(input)
    }
    if input.scenario.hasPrefix("arkavo/transport-equivalence/") {
        return await performTransportEquivalence(input)
    }
    // iroh discovery (iroh-discovery-v1 §5, IROH-HARNESS.md): Swift = relay only.
    // The client resolves the card over HTTP, reads {nodeId, gateway}, and drives
    // the op through the relay gateway (no iroh awareness).
    if input.scenario.hasPrefix("arkavo/discovery/") {
        return await performDiscoveryOp(input)
    }
    let transport = makeTransport(timeoutMs: input.timeoutMs)
    do {
        switch input.op {
        case "SendMessage":
            let request = try decodeParams(SendMessageRequest.self, from: input.params)
            let client = await makeClient(baseUrl: input.baseUrl, transport: transport)
            return resultOutcome(try encodeToJSONValue(try await client.sendMessage(request)))

        case "GetTask":
            let request = try decodeParams(GetTaskRequest.self, from: input.params)
            let client = await makeClient(baseUrl: input.baseUrl, transport: transport)
            return resultOutcome(try encodeToJSONValue(try await client.getTask(request)))

        case "ListTasks":
            let request = try decodeParams(ListTasksRequest.self, from: input.params)
            let client = await makeClient(baseUrl: input.baseUrl, transport: transport)
            return resultOutcome(try encodeToJSONValue(try await client.listTasks(request)))

        case "CancelTask":
            let request = try decodeParams(CancelTaskRequest.self, from: input.params)
            let client = await makeClient(baseUrl: input.baseUrl, transport: transport)
            return resultOutcome(try encodeToJSONValue(try await client.cancelTask(request)))

        case "CreateTaskPushNotificationConfig":
            let config = try decodeParams(TaskPushNotificationConfig.self, from: input.params)
            let client = await makeClient(baseUrl: input.baseUrl, transport: transport)
            return resultOutcome(
                try encodeToJSONValue(try await client.createTaskPushNotificationConfig(config)))

        case "GetExtendedAgentCard":
            let request = try decodeParams(GetExtendedAgentCardRequest.self, from: input.params)
            let client = await makeClient(baseUrl: input.baseUrl, transport: transport)
            return resultOutcome(
                try encodeToJSONValue(try await client.getExtendedAgentCard(request)))

        case "SendStreamingMessage":
            let request = try decodeParams(SendMessageRequest.self, from: input.params)
            let client = await makeClient(baseUrl: input.baseUrl, transport: transport)
            return await streamOutcome(client.sendStreamingMessage(request))

        case "SubscribeToTask":
            let request = try decodeParams(SubscribeToTaskRequest.self, from: input.params)
            let client = await makeClient(baseUrl: input.baseUrl, transport: transport)
            return await streamOutcome(client.subscribeToTask(request))

        case "ResolveCard":
            guard let url = URL(string: input.baseUrl + A2AProtocol.agentCardWellKnownPath)
            else {
                return harnessErrorOutcome("invalid baseUrl \(input.baseUrl)")
            }
            let card = try await AgentCardResolver(transport: transport).resolve(url: url)
            return resultOutcome(try encodeToJSONValue(card), kind: "card")

        case "SelectInterface":
            guard let url = URL(string: input.baseUrl + A2AProtocol.agentCardWellKnownPath)
            else {
                return harnessErrorOutcome("invalid baseUrl \(input.baseUrl)")
            }
            let card = try await AgentCardResolver(transport: transport).resolve(url: url)
            guard let interface = AgentCardResolver.selectInterface(from: card) else {
                return .object([
                    "kind": .string("error"),
                    "errorCode": .null,
                    "errorMessage": .string("no compatible interface"),
                ])
            }
            return resultOutcome(try encodeToJSONValue(interface), kind: "interface")

        case "RawRequest":
            return await rawRequestOutcome(input)

        default:
            return .object([
                "kind": .string("unsupported"),
                "detail": .string("client harness: unknown op \(input.op)"),
            ])
        }
    } catch {
        return errorOutcome(error)
    }
}

/// Races the op against a deadline. The op's transport carries its own
/// URLSession timeouts (~timeoutMs), so the losing branch cannot hang the
/// group for long after the deadline fires.
func performOpWithTimeout(_ input: InputLine) async -> JSONValue {
    await withTaskGroup(of: JSONValue?.self) { group in
        group.addTask { await performOp(input) }
        group.addTask {
            try? await Task.sleep(nanoseconds: UInt64(input.timeoutMs) * 1_000_000)
            return nil
        }
        let first = await group.next() ?? nil
        group.cancelAll()
        return first
            ?? harnessErrorOutcome("client harness timed out after \(input.timeoutMs)ms")
    }
}

// MARK: - Main loop

func emit(scenario: String, outcome: JSONValue, durationMs: Double) {
    let line: JSONValue = .object([
        "scenario": .string(scenario),
        "outcome": outcome,
        "durationMs": .number((durationMs * 100).rounded() / 100),
        "impl": .object([
            "name": .string(implName),
            "version": .string(implVersion),
        ]),
    ])
    do {
        let data = try A2AJSON.encoder().encode(line)
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data("\n".utf8))
    } catch {
        // Last-resort fallback; keeps the runner from hanging on a lost line.
        let fallback =
            "{\"scenario\": \"\(scenario)\", \"outcome\": {\"kind\": \"harness-error\", "
            + "\"detail\": \"outcome encode failed\"}, \"durationMs\": 0, "
            + "\"impl\": {\"name\": \"\(implName)\", \"version\": \"\(implVersion)\"}}\n"
        FileHandle.standardOutput.write(Data(fallback.utf8))
    }
}

while let line = readLine(strippingNewline: true) {
    if line.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
        continue
    }
    let start = DispatchTime.now()
    guard let input = InputLine(parsing: line) else {
        logError("unparsable input line: \(line)")
        emit(
            scenario: "unknown",
            outcome: harnessErrorOutcome("unparsable input line"),
            durationMs: 0)
        continue
    }
    let outcome = await performOpWithTimeout(input)
    let durationMs =
        Double(DispatchTime.now().uptimeNanoseconds - start.uptimeNanoseconds) / 1_000_000
    emit(scenario: input.scenario, outcome: outcome, durationMs: durationMs)
}
