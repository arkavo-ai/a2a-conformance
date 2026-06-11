// arkavo-ext: scenario-keyed identity ops (IDENTITY-HARNESS.md).
//
// - cwt-auth-success: resolve the card, read the target DID from the
//   aia-identity extension params (fallback: manifest serverDid), present a
//   fresh CWT per call via ArkavoA2AIdentity's CwtAuthenticator.
// - cwt-expired / cwt-wrong-audience: mint one deliberately bad CWT with
//   signCWT and attach it via a static-header RequestAuthenticator; the
//   SDK surfaces the 401 as A2ATransportError.httpError(statusCode: 401, ...)
//   -> outcome error with errorCode null and the status in errorMessage.
// - card-signature-valid / -tampered: verify the exact served card bytes
//   with verifyCard(.requireIdentity); verified -> card outcome (SDK
//   re-encode), anything else -> fail-closed error outcome.

import A2A
import A2AClient
import ArkavoA2AIdentity
import Crypto
import Foundation

#if canImport(FoundationNetworking)
    import FoundationNetworking
#endif

let identityFixtures: Result<IdentityFixtures, any Error> = Result { try IdentityFixtures.load() }

/// Presents one fixed (deliberately bad) credential on every request.
struct StaticBearerAuthenticator: RequestAuthenticator {
    let headerValue: String

    func authenticate(_ request: inout URLRequest) async throws {
        request.setValue(headerValue, forHTTPHeaderField: "Authorization")
    }
}

/// TEST-ONLY throwaway DID for the wrong-audience credential, derived from
/// the same constant scalar as the fixture generator's.
func throwawayDid() throws -> String {
    let key = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 0x42, count: 32))
    return DIDKey.encode(key.publicKey)
}

func randomCti() -> Data {
    var generator = SystemRandomNumberGenerator()
    var cti = Data(capacity: 16)
    for _ in 0..<2 {
        withUnsafeBytes(of: generator.next().littleEndian) { cti.append(contentsOf: $0) }
    }
    return cti
}

/// The target DID (`aud`): the aia-identity extension's `params.did` on the
/// resolved card, falling back to the manifest serverDid.
func resolveAud(_ input: InputLine, transport: URLSessionTransport, fx: IdentityFixtures) async
    -> String
{
    if let url = URL(string: input.baseUrl + A2AProtocol.agentCardWellKnownPath),
        let card = try? await AgentCardResolver(transport: transport).resolve(url: url),
        let ext = card.capabilities.extensions.first(where: {
            $0.uri == IdentityFixtures.extensionURI
        }),
        let did = ext.params?["did"]?.stringValue
    {
        return did
    }
    return fx.serverDid
}

/// Card-first client construction (same rule as makeClient) with a
/// RequestAuthenticator attached.
func makeAuthenticatedClient(
    _ input: InputLine, transport: URLSessionTransport, authenticator: any RequestAuthenticator
) async -> A2AClient {
    if let cardURL = URL(string: input.baseUrl + A2AProtocol.agentCardWellKnownPath),
        let card = try? await AgentCardResolver(transport: transport).resolve(url: cardURL),
        let interface = AgentCardResolver.selectInterface(from: card),
        sameAuthority(interface.url, input.baseUrl),
        let client = try? A2AClient(card: card, transport: transport, authenticator: authenticator)
    {
        return client
    }
    let synthetic = AgentInterface(
        url: input.baseUrl,
        protocolBinding: AgentInterface.Binding.jsonrpc,
        protocolVersion: A2AProtocol.version)
    return A2AClient(interface: synthetic, transport: transport, authenticator: authenticator)
}

func performIdentityOp(_ input: InputLine) async -> JSONValue {
    let fx: IdentityFixtures
    switch identityFixtures {
    case .success(let loaded): fx = loaded
    case .failure(let error):
        return harnessErrorOutcome("identity fixtures unavailable: \(error)")
    }
    let suffix = String(input.scenario.dropFirst("arkavo/identity/".count))
    let transport = makeTransport(timeoutMs: input.timeoutMs)
    do {
        switch suffix {
        case "cwt-auth-success":
            let request = try decodeParams(SendMessageRequest.self, from: input.params)
            let aud = await resolveAud(input, transport: transport, fx: fx)
            let authenticator = CwtAuthenticator(
                signingKey: fx.issuerPrivateKey,
                issuer: fx.iss,
                subjectDID: fx.clientDid,
                audienceDID: aud,
                confirmationKey: CnfKey(publicKey: fx.clientPublicKey),
                ttl: 240)
            let client = await makeAuthenticatedClient(
                input, transport: transport, authenticator: authenticator)
            return resultOutcome(try encodeToJSONValue(try await client.sendMessage(request)))

        case "cwt-expired", "cwt-wrong-audience":
            let request = try decodeParams(SendMessageRequest.self, from: input.params)
            let now = Int64(Date().timeIntervalSince1970)
            // Expired: 60 s past expiry, beyond the ±30 s skew allowance.
            let (aud, iat, exp): (String, Int64, Int64) =
                suffix == "cwt-expired"
                ? (fx.serverDid, now - 300, now - 60)
                : (try throwawayDid(), now, now + 240)
            let claims = CwtClaims(
                iss: fx.iss, sub: fx.clientDid, aud: aud, exp: exp, iat: iat,
                cti: randomCti(), cnf: CnfKey(publicKey: fx.clientPublicKey))
            let token = try signCWT(claims: claims, key: fx.issuerPrivateKey)
            let authenticator = StaticBearerAuthenticator(
                headerValue: "Bearer " + base64URLEncode(token))
            let client = await makeAuthenticatedClient(
                input, transport: transport, authenticator: authenticator)
            // Expected to throw A2ATransportError.httpError(statusCode: 401);
            // the catch below maps it to errorCode null + descriptive message.
            return resultOutcome(try encodeToJSONValue(try await client.sendMessage(request)))

        case "card-signature-valid", "card-signature-tampered":
            guard let url = URL(string: input.baseUrl + A2AProtocol.agentCardWellKnownPath) else {
                return harnessErrorOutcome("invalid baseUrl \(input.baseUrl)")
            }
            // Verify over the exact served bytes (the JWS covers their
            // canonical form — SDK re-decode first could legally drop
            // unknown fields).
            let (data, _) = try await URLSession.shared.data(for: URLRequest(url: url))
            let tree = try A2AJSON.decoder().decode(JSONValue.self, from: data)
            let verification = try verifyCard(tree, mode: .requireIdentity)
            guard case .verified(let did) = verification else {
                return .object([
                    "kind": .string("error"),
                    "errorCode": .null,
                    "errorMessage": .string("card not identity-verified: \(verification)"),
                ])
            }
            logError("card identity-verified (did \(did))")
            let card = try await AgentCardResolver(transport: transport).resolve(url: url)
            return resultOutcome(try encodeToJSONValue(card), kind: "card")

        default:
            return .object([
                "kind": .string("unsupported"),
                "detail": .string("unknown arkavo/identity scenario \(suffix)"),
            ])
        }
    } catch {
        // CardVerificationError (fail closed) and A2ATransportError.httpError
        // both land here: errorCode null, description in errorMessage.
        return errorOutcome(error)
    }
}
