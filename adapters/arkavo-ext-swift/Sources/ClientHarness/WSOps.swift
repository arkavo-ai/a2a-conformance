// arkavo-ext-swift WebSocket binding (ws-binding-v1, WS-HARNESS.md).
//
// a2a-swift's WS layer is client-only this phase: this harness drives
// A2AWSClient (ArkavoA2AWS) for `arkavo/ws/*` and
// `arkavo/transport-equivalence/*` scenarios against a Rust ext WS server. The
// client resolves the card, selects its JSONRPC-WS interface, connects the SDK
// WS client to the real ws port, drives the op, and emits the SAME outcome
// shapes the HTTP path emits (result / stream / error).
//
// transport-equivalence/*: the op runs over HTTP/JSON AND over WS, and the
// decoded results are asserted identical IN THE HARNESS; the WS run's outcome
// is emitted. send-message-equivalent adds a WS/CBOR leg (the canonical
// binary frame). ws-subprotocol-negotiation asserts the negotiated codec via
// the connection's exposed `negotiatedCodec`.
//
// The WS connection is opened inside each scenario's op handler and closed at
// the end of that handler (per-op lifecycle). All multi-step WS behaviours
// (re-subscribe, concurrent multiplexing) happen WITHIN a single op-line
// handler, so the connection never needs to persist across op lines — the
// harness has no /select signal. Per-op open/close honours WS-HARNESS.md's
// "torn down" deterministically and removes any stale-connection risk.

import A2A
import A2AClient
import ArkavoA2AWS
import ArkavoA2AWire
import Foundation

#if canImport(FoundationNetworking)
    import FoundationNetworking
#endif

/// Open-form protocol binding for the WebSocket interface (ws-binding-v1 §1).
let transportProtocolJSONRPCWS = "JSONRPC-WS"

/// Carries a ready-to-emit failure outcome through `Result`'s `Error` slot.
struct OutcomeError: Error {
    let outcome: JSONValue
}

/// Open a fresh WS connection for one scenario op, offering `codecs` in
/// preference order (ws-binding-v1 §2). The caller closes it at the end of the
/// op handler (per-op lifecycle — see file header).
func openWSClient(wsURL: URL, offering codecs: [FrameCodec], timeoutMs: Int) async throws
    -> A2AWSClient
{
    let configuration = URLSessionConfiguration.ephemeral
    let seconds = Double(timeoutMs) / 1000.0
    configuration.timeoutIntervalForRequest = max(seconds, 1)
    configuration.timeoutIntervalForResource = max(seconds, 1)
    return try await A2AWSClient.connect(
        url: wsURL,
        preferredCodecs: codecs,
        configuration: configuration)
}

/// Resolve the card's JSONRPC-WS interface URL (ws-binding-v1 §1 — interface
/// selection against the real card-advertised endpoint).
func resolveWSURL(baseURL: String, transport: URLSessionTransport) async -> Result<
    URL, OutcomeError
> {
    guard let cardURL = URL(string: baseURL + A2AProtocol.agentCardWellKnownPath) else {
        return .failure(OutcomeError(outcome: harnessErrorOutcome("invalid baseUrl \(baseURL)")))
    }
    do {
        let card = try await AgentCardResolver(transport: transport).resolve(url: cardURL)
        guard
            let iface = card.supportedInterfaces.first(where: {
                $0.protocolBinding.caseInsensitiveCompare(transportProtocolJSONRPCWS)
                    == .orderedSame
            }),
            let url = URL(string: iface.url)
        else {
            return .failure(
                OutcomeError(
                    outcome: .object([
                        "kind": .string("error"), "errorCode": .null,
                        "errorMessage": .string("card has no JSONRPC-WS interface"),
                    ])))
        }
        return .success(url)
    } catch {
        return .failure(OutcomeError(outcome: errorOutcome(error)))
    }
}

/// The codec offer (preference order) for a ws scenario. CBOR is preferred so
/// the canonical CBOR binary-frame path is exercised wherever the server
/// negotiates it. `ws-subprotocol-negotiation` offers BOTH codecs in preference
/// order ([cbor, json], WS-HARNESS.md) so it genuinely tests preference-order
/// negotiation, then asserts the server selects cbor.
func wsOffer(for scenario: String) -> [FrameCodec] {
    switch scenario {
    case "arkavo/ws/ws-subprotocol-negotiation":
        return [.cbor, .json]
    default:
        return [.cbor, .json]
    }
}

/// Run one op over a connected A2AWSClient, producing the runner outcome value
/// (result / stream / error) — the same shapes the HTTP path emits.
func wsRunOp(_ client: A2AWSClient, _ input: InputLine) async -> JSONValue {
    do {
        switch input.op {
        case "SendMessage":
            let request = try decodeParams(SendMessageRequest.self, from: input.params)
            return resultOutcome(try encodeToJSONValue(try await client.sendMessage(request)))
        case "GetTask":
            let request = try decodeParams(GetTaskRequest.self, from: input.params)
            return resultOutcome(try encodeToJSONValue(try await client.getTask(request)))
        case "SendStreamingMessage":
            let request = try decodeParams(SendMessageRequest.self, from: input.params)
            return await streamOutcome(client.sendStreamingMessage(request))
        case "SubscribeToTask":
            let request = try decodeParams(SubscribeToTaskRequest.self, from: input.params)
            return await streamOutcome(client.subscribeToTask(request))
        default:
            return .object([
                "kind": .string("unsupported"),
                "detail": .string("ws path does not support op \(input.op)"),
            ])
        }
    } catch {
        return errorOutcome(error)
    }
}

/// `arkavo/ws/*`: connect the SDK WS client to the card's JSONRPC-WS interface,
/// drive the op, emit the HTTP-equivalent outcome shape.
func performWSOp(_ input: InputLine) async -> JSONValue {
    let transport = makeTransport(timeoutMs: input.timeoutMs)
    let wsURL: URL
    switch await resolveWSURL(baseURL: input.baseUrl, transport: transport) {
    case .success(let url): wsURL = url
    case .failure(let error): return error.outcome
    }

    let codecs = wsOffer(for: input.scenario)
    let client: A2AWSClient
    do {
        client = try await openWSClient(
            wsURL: wsURL, offering: codecs, timeoutMs: input.timeoutMs)
    } catch {
        return .object([
            "kind": .string("error"), "errorCode": .null,
            "errorMessage": .string("ws connect failed: \(error)"),
        ])
    }
    // Per-op lifecycle: drive the scenario, then deterministically tear the
    // connection down before returning (the harness analogue of /select).
    let outcome = await driveWSScenario(client, input)
    await client.close()
    return outcome
}

/// Drive one ws/* scenario over an already-connected client. Multi-step
/// behaviours (re-subscribe, concurrent multiplexing) run entirely here, within
/// a single op handler, on one live socket.
private func driveWSScenario(_ client: A2AWSClient, _ input: InputLine) async -> JSONValue {
    // ws-subprotocol-negotiation: offered [cbor, json] (preference order); the
    // server MUST select cbor. Assert via the SDK's exposed negotiatedCodec.
    if input.scenario == "arkavo/ws/ws-subprotocol-negotiation" {
        let negotiated = await client.connection.negotiatedCodec
        guard negotiated == .cbor else {
            return .object([
                "kind": .string("error"), "errorCode": .null,
                "errorMessage": .string(
                    "subprotocol negotiation: offered [cbor, json], expected cbor, got "
                        + negotiated.subprotocol),
            ])
        }
        logError("ws negotiated subprotocol \(negotiated.subprotocol)")
        return await wsRunOp(client, input)
    }

    // ws-resubscribe-same-socket: subscribe, consume, then re-subscribe on the
    // SAME live socket with a fresh id (ws-binding-v1 §3). Emit the second run.
    if input.scenario == "arkavo/ws/ws-resubscribe-same-socket" {
        let first = await wsRunOp(client, input)
        if first.objectValue?["kind"]?.stringValue != "stream" {
            return first
        }
        logError("ws re-subscribing on the same socket (fresh id)")
        return await wsRunOp(client, input)
    }

    // ws-concurrent-streams-multiplexed: open TWO streaming exchanges on one
    // connection concurrently; both must complete in per-id order. Emit the
    // first stream's outcome (both share the scenario's expected order).
    if input.scenario == "arkavo/ws/ws-concurrent-streams-multiplexed" {
        async let a = wsRunOp(client, input)
        async let b = wsRunOp(client, input)
        let (first, second) = await (a, b)
        if second.objectValue?["kind"]?.stringValue != "stream" {
            return second
        }
        return first
    }

    return await wsRunOp(client, input)
}

/// The comparable part of an outcome for transport equivalence: the decoded
/// result value, or the ordered stream event list.
func comparableOutcome(_ outcome: JSONValue) -> JSONValue {
    switch outcome.objectValue?["kind"]?.stringValue {
    case "result": return outcome.objectValue?["value"] ?? .null
    case "stream": return outcome.objectValue?["events"] ?? .null
    default: return outcome
    }
}

/// `arkavo/transport-equivalence/*`: run the op over HTTP/JSON AND over WS, and
/// assert the decoded results are identical in the harness. send-message adds a
/// WS/CBOR leg (the canonical binary frame). The WS run's outcome is emitted.
func performTransportEquivalence(_ input: InputLine) async -> JSONValue {
    // HTTP/JSON leg (the equivalence oracle): the ordinary HTTP path.
    let httpOutcome = await performHTTPOp(input)
    let comparableHTTP = comparableOutcome(httpOutcome)

    let transport = makeTransport(timeoutMs: input.timeoutMs)
    let wsURL: URL
    switch await resolveWSURL(baseURL: input.baseUrl, transport: transport) {
    case .success(let url): wsURL = url
    case .failure(let error): return error.outcome
    }

    // WS legs: JSON always; CBOR too for the unary send-message case (the
    // binary canonical-CBOR frame path).
    let legs: [(String, FrameCodec)]
    if input.op == "SendMessage" {
        legs = [("ws/json", .json), ("ws/cbor", .cbor)]
    } else {
        legs = [("ws/json", .json)]
    }

    var wsOutcome: JSONValue = .null
    for (label, codec) in legs {
        // Per-op lifecycle: one fresh connection per leg, closed before moving on.
        let client: A2AWSClient
        do {
            client = try await openWSClient(
                wsURL: wsURL, offering: [codec], timeoutMs: input.timeoutMs)
        } catch {
            return .object([
                "kind": .string("error"), "errorCode": .null,
                "errorMessage": .string("ws connect failed (\(label)): \(error)"),
            ])
        }
        let negotiated = await client.connection.negotiatedCodec
        guard negotiated == codec else {
            await client.close()
            return .object([
                "kind": .string("error"), "errorCode": .null,
                "errorMessage": .string(
                    "transport-equivalence \(label): server did not negotiate \(codec.subprotocol)"
                ),
            ])
        }
        let outcome = await wsRunOp(client, input)
        await client.close()
        let comparableWS = comparableOutcome(outcome)
        guard comparableWS == comparableHTTP else {
            return .object([
                "kind": .string("error"), "errorCode": .null,
                "errorMessage": .string(
                    "transport-equivalence divergence (\(label) vs http/json)"),
            ])
        }
        logError("transport-equivalence \(label) (\(codec.subprotocol)) ≡ http/json")
        wsOutcome = outcome
    }
    return wsOutcome
}

/// The ordinary HTTP path for one op, returning the runner outcome value. Used
/// as the transport-equivalence oracle (mirrors performOp's HTTP dispatch).
func performHTTPOp(_ input: InputLine) async -> JSONValue {
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
        case "SendStreamingMessage":
            let request = try decodeParams(SendMessageRequest.self, from: input.params)
            let client = await makeClient(baseUrl: input.baseUrl, transport: transport)
            return await streamOutcome(client.sendStreamingMessage(request))
        case "SubscribeToTask":
            let request = try decodeParams(SubscribeToTaskRequest.self, from: input.params)
            let client = await makeClient(baseUrl: input.baseUrl, transport: transport)
            return await streamOutcome(client.subscribeToTask(request))
        default:
            return .object([
                "kind": .string("unsupported"),
                "detail": .string("transport-equivalence http oracle: unsupported op \(input.op)"),
            ])
        }
    } catch {
        return errorOutcome(error)
    }
}
