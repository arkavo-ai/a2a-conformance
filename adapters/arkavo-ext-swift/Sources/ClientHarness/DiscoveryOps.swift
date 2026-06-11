// arkavo-ext-swift iroh discovery (iroh-discovery-v1 §5, IROH-HARNESS.md).
//
// Swift = RELAY ONLY (native iroh is Rust-first, spec §7.4). For
// arkavo/discovery/* the client resolves the agent card over plain HTTP
// (baseUrl = the capture proxy), reads the iroh-discovery extension params
// `{nodeId, gateway}`, and drives the op through the relay HTTPS gateway via
// ArkavoA2ADiscovery's IrohGatewayResolver — REUSING the normal HTTP A2AClient
// against `<gateway>/<nodeId>/`, with ZERO iroh awareness.
//
//   card-by-node-id          → resolveCard via the gateway; assert it matches
//                              the HTTP well-known card; emit kind=card.
//   relay-fallback-equivalence → run the op via the relay; emit the result.
//                              (The equivalence oracle is the Rust self-pair's
//                              native==relay comparison; Swift proves the relay
//                              leg yields the right result — IROH-HARNESS.md.)

import A2A
import A2AClient
import ArkavoA2ADiscovery
import Foundation

#if canImport(FoundationNetworking)
    import FoundationNetworking
#endif

/// Resolve the HTTP card and read the iroh-discovery extension params
/// `(nodeID, gateway)`. The gateway is REQUIRED here (the Rust server always
/// advertises it); the spec default would point at the public internet.
private func resolveIrohParams(baseURL: String, transport: URLSessionTransport) async
    -> Result<(card: AgentCard, nodeID: String, gateway: URL), OutcomeError>
{
    guard let url = URL(string: baseURL + A2AProtocol.agentCardWellKnownPath) else {
        return .failure(OutcomeError(outcome: harnessErrorOutcome("invalid baseUrl \(baseURL)")))
    }
    let card: AgentCard
    do {
        card = try await AgentCardResolver(transport: transport).resolve(url: url)
    } catch {
        return .failure(OutcomeError(outcome: errorOutcome(error)))
    }
    guard let params = IrohGatewayResolver.irohParams(in: card) else {
        return .failure(
            OutcomeError(
                outcome: .object([
                    "kind": .string("error"), "errorCode": .null,
                    "errorMessage": .string("card has no iroh-discovery extension / nodeId"),
                ])))
    }
    guard let gateway = params.gateway else {
        return .failure(
            OutcomeError(
                outcome: .object([
                    "kind": .string("error"), "errorCode": .null,
                    "errorMessage": .string("iroh-discovery params omit gateway"),
                ])))
    }
    return .success((card, params.nodeID, gateway))
}

/// JCS-ish comparison: both cards are the same SDK's AgentCard, so compare their
/// canonical JSON encodings.
private func cardsMatch(_ a: AgentCard, _ b: AgentCard) -> Bool {
    guard let ea = try? A2AJSON.encoder().encode(a),
        let eb = try? A2AJSON.encoder().encode(b)
    else { return false }
    return ea == eb
}

/// `arkavo/discovery/card-by-node-id`: resolve the card through the gateway and
/// assert it matches the HTTP well-known card; emit the resolved card.
private func performCardByNodeID(_ input: InputLine, transport: URLSessionTransport) async
    -> JSONValue
{
    let resolved = await resolveIrohParams(baseURL: input.baseUrl, transport: transport)
    let (httpCard, nodeID, gateway): (AgentCard, String, URL)
    switch resolved {
    case .success(let v): (httpCard, nodeID, gateway) = v
    case .failure(let e): return e.outcome
    }

    let resolver = IrohGatewayResolver(gateway: gateway, transport: transport)
    do {
        let gatewayCard = try await resolver.resolveCard(nodeID: nodeID)
        guard cardsMatch(gatewayCard, httpCard) else {
            return .object([
                "kind": .string("error"), "errorCode": .null,
                "errorMessage": .string(
                    "relay gateway card does not match HTTP well-known card after canonicalization"),
            ])
        }
        logError("iroh relay card-by-node-id matched (node \(nodeID))")
        return resultOutcome(try encodeToJSONValue(gatewayCard), kind: "card")
    } catch {
        return errorOutcome(error)
    }
}

/// `arkavo/discovery/relay-fallback-equivalence`: run the op via the relay
/// gateway and emit the result (the equivalence oracle is the Rust self-pair).
private func performRelayEquivalence(_ input: InputLine, transport: URLSessionTransport) async
    -> JSONValue
{
    guard input.op == "SendMessage" else {
        return .object([
            "kind": .string("unsupported"),
            "detail": .string(
                "relay-fallback-equivalence is pinned to SendMessage, got \(input.op)"),
        ])
    }
    let resolved = await resolveIrohParams(baseURL: input.baseUrl, transport: transport)
    let (nodeID, gateway): (String, URL)
    switch resolved {
    case .success(let v): (_, nodeID, gateway) = v
    case .failure(let e): return e.outcome
    }

    let resolver = IrohGatewayResolver(gateway: gateway, transport: transport)
    do {
        let request = try decodeParams(SendMessageRequest.self, from: input.params)
        // makeClient resolves the card through the gateway first (proving the
        // relay-served card is reachable), then builds a normal A2AClient bound
        // to <gateway>/<nodeId>/ with zero iroh awareness.
        let client = try await resolver.makeClient(nodeID: nodeID)
        let response = try await client.sendMessage(request)
        logError("iroh relay-fallback-equivalence relay leg ok (node \(nodeID))")
        return resultOutcome(try encodeToJSONValue(response))
    } catch {
        return errorOutcome(error)
    }
}

/// Entry point for `arkavo/discovery/*` (relay only).
func performDiscoveryOp(_ input: InputLine) async -> JSONValue {
    let transport = makeTransport(timeoutMs: input.timeoutMs)
    let suffix = String(input.scenario.dropFirst("arkavo/discovery/".count))
    switch suffix {
    case "card-by-node-id":
        return await performCardByNodeID(input, transport: transport)
    case "relay-fallback-equivalence":
        return await performRelayEquivalence(input, transport: transport)
    default:
        return .object([
            "kind": .string("unsupported"),
            "detail": .string("unknown arkavo/discovery scenario: \(suffix)"),
        ])
    }
}
