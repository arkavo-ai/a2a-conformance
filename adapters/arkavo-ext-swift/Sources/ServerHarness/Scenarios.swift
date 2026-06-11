// Scenario corpus loading and the per-run mutable state (selected scenario,
// served card, observation buffer). Scenario JSON is parsed with
// JSONSerialization; only `id`, `client.op`, and the `server` section matter
// to the server harness. Scripted payloads are kept as raw JSON `Data` and
// decoded into SDK types at request time so the SDK's codecs stay in the loop.

import A2A
import ArkavoA2AIdentity
import ArkavoA2ATDF
import Crypto
import Foundation

/// The script for one scenario, as far as the server harness is concerned.
struct ScenarioScript: Sendable {
    var id: String
    var op: String
    /// `server.respond` re-serialized JSON (decoded into the SDK result type at request time).
    var respond: Data?
    /// `server.error` re-serialized JSON (decoded into `JSONRPCErrorObject`).
    var error: Data?
    /// `server.sse` frames, each re-serialized JSON (decoded into `StreamResponse`).
    var sse: [Data]?
    /// `server.rawResult`: a JSON *text* to be emitted verbatim as the result
    /// member, bypassing the typed dispatcher (client-tolerance fixtures).
    var rawResult: Data?
    /// `server.card` JSON text, pre-`{{baseUrl}}`-substitution.
    var cardText: String?
}

enum ScenarioLoadError: Error, CustomStringConvertible {
    case notADirectory(String)
    var description: String {
        switch self {
        case .notADirectory(let path): return "--scenarios is not a directory: \(path)"
        }
    }
}

enum ScenarioCorpus {
    static func load(from directory: String) throws -> [String: ScenarioScript] {
        // Resolve symlinks so a linked scenarios dir still enumerates.
        let url = URL(fileURLWithPath: directory, isDirectory: true).resolvingSymlinksInPath()
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory),
            isDirectory.boolValue
        else {
            throw ScenarioLoadError.notADirectory(directory)
        }
        var corpus: [String: ScenarioScript] = [:]
        guard
            let enumerator = FileManager.default.enumerator(
                at: url, includingPropertiesForKeys: nil)
        else {
            return corpus
        }
        for case let fileURL as URL in enumerator where fileURL.pathExtension == "json" {
            guard let script = try? parse(fileURL: fileURL) else {
                FileHandle.standardError.write(
                    Data("server-harness: skipping unparsable scenario \(fileURL.path)\n".utf8))
                continue
            }
            corpus[script.id] = script
        }
        return corpus
    }

    private static func parse(fileURL: URL) throws -> ScenarioScript? {
        let data = try Data(contentsOf: fileURL)
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
            let id = object["id"] as? String,
            let client = object["client"] as? [String: Any],
            let op = client["op"] as? String
        else {
            return nil
        }
        let server = object["server"] as? [String: Any] ?? [:]
        func section(_ key: String) -> Data? {
            guard let value = server[key] else { return nil }
            return try? JSONSerialization.data(
                withJSONObject: value, options: [.fragmentsAllowed, .sortedKeys])
        }
        var script = ScenarioScript(id: id, op: op)
        script.respond = section("respond")
        script.error = section("error")
        if let frames = server["sse"] as? [Any] {
            script.sse = frames.compactMap {
                try? JSONSerialization.data(withJSONObject: $0, options: [.sortedKeys])
            }
        }
        if let rawResult = server["rawResult"] as? String {
            script.rawResult = Data(rawResult.utf8)
        }
        if let cardData = section("card") {
            script.cardText = String(decoding: cardData, as: UTF8.self)
        }
        return script
    }
}

/// The single piece of mutable harness state: which scenario is armed, what
/// card is served, and the params of the last request the SDK handed us.
actor ScenarioState {
    private let corpus: [String: ScenarioScript]
    private let publicBaseUrl: String
    private let identity: IdentityFixtures
    private let tdf: TDFFixtures
    private let defaultCard: Data
    /// shape-(b) blob store (hex -> ciphertext bytes), written payload-first.
    private var b3Blobs: [String: Data] = [:]
    private var selected: ScenarioScript?
    private var servedCard: Data
    private var observedParams: Data?
    /// True while an `arkavo/identity/cwt-*` scenario is selected (the POST
    /// route then enforces Bearer-CWT auth before dispatching to the SDK).
    private var cwtArmed = false
    /// True while an `arkavo/policy/*` scenario is selected: dispatch routes
    /// through the GatedRequestHandler (POLICY-HARNESS.md server arming).
    private var gateArmed = false
    /// True while `arkavo/policy/gate-deny-task-op-32099` is selected: the
    /// harness composite evaluator denies every task-management op (TM-001).
    private var taskOpDenyArmed = false

    init(
        corpus: [String: ScenarioScript],
        publicBaseUrl: String,
        identity: IdentityFixtures,
        tdf: TDFFixtures
    ) {
        self.corpus = corpus
        self.publicBaseUrl = publicBaseUrl
        self.identity = identity
        self.tdf = tdf
        let card = AgentCard(
            name: "Arkavo Conformance Agent",
            description: "Scripted a2a-swift agent for conformance testing.",
            supportedInterfaces: [
                AgentInterface(
                    url: publicBaseUrl,
                    protocolBinding: AgentInterface.Binding.jsonrpc,
                    protocolVersion: A2AProtocol.version)
            ],
            version: "0.1.0",
            // The default card always advertises the aia-identity extension
            // with params.did = serverDid (required: false — degradation
            // contract): cwt-auth-success clients read the aud from here,
            // vanilla peers ignore it.
            capabilities: AgentCapabilities(
                streaming: true,
                extensions: [
                    AgentExtension(
                        uri: IdentityFixtures.extensionURI,
                        required: false,
                        params: [
                            "did": .string(identity.serverDid),
                            "issuer": .string(identity.iss),
                        ]),
                    // tdf-parts-v1 advertisement (required: false — degradation
                    // contract): schemes/kas/gateway per §1.
                    AgentExtension(
                        uri: TDFExtension.uri,
                        required: false,
                        params: tdfExtensionParams()),
                ]),
            defaultInputModes: ["text/plain"],
            defaultOutputModes: ["text/plain"],
            skills: [
                AgentSkill(
                    id: "conformance",
                    name: "Conformance",
                    description: "Scripted conformance agent.",
                    tags: ["conformance"])
            ])
        // The default card always round-trips; fall back to raw JSON if not.
        self.defaultCard =
            (try? A2AJSON.encoder().encode(card))
            ?? Data("{\"name\":\"Arkavo Conformance Agent\"}".utf8)
        self.servedCard = self.defaultCard
    }

    func authArmed() -> Bool {
        cwtArmed
    }

    func policyArmed() -> Bool {
        gateArmed
    }

    func policyTaskOpDenyArmed() -> Bool {
        taskOpDenyArmed
    }

    /// Builds the served bytes for the card-signature scenarios: scenario
    /// card with `{{baseUrl}}` substituted, the identity extension
    /// advertised, signed with the test-server-agent key — and, for
    /// `-tampered`, one character of `description` mutated AFTER signing.
    /// The exact serialized bytes are served (no SDK re-encode: the JWS
    /// covers their canonical form).
    private func buildSignedCard(cardText: String, tampered: Bool) throws -> Data {
        let substituted = cardText.replacingOccurrences(of: "{{baseUrl}}", with: publicBaseUrl)
        var card = try A2AJSON.decoder().decode(JSONValue.self, from: Data(substituted.utf8))
        guard var members = card.objectValue else {
            throw IdentityFixtureError.malformedManifest("scenario card is not an object")
        }
        var capabilities = members["capabilities"]?.objectValue ?? [:]
        var extensions = capabilities["extensions"]?.arrayValue ?? []
        extensions.append(
            .object([
                "uri": .string(IdentityFixtures.extensionURI),
                "required": .bool(false),
                "params": .object([
                    "did": .string(identity.serverDid),
                    "issuer": .string(identity.iss),
                ]),
            ]))
        capabilities["extensions"] = .array(extensions)
        members["capabilities"] = .object(capabilities)
        card = .object(members)

        let fragment = String(identity.serverDid.dropFirst("did:key:".count))
        let kid = "\(identity.serverDid)#\(fragment)"
        try signCard(&card, key: identity.serverPrivateKey, kidDidUrl: kid)

        if tampered {
            guard var signed = card.objectValue,
                let description = signed["description"]?.stringValue, !description.isEmpty
            else {
                throw IdentityFixtureError.malformedManifest("card has no description to tamper")
            }
            let last = description.last!
            let mutated = String(description.dropLast()) + (last == "!" ? "?" : "!")
            signed["description"] = .string(mutated)
            card = .object(signed)
        }
        return CanonicalJSON.serialize(card)
    }

    /// Arms a scenario. Returns (ok, reason). This harness can script every
    /// scenario kind in the corpus (respond/error/sse/rawResult/card and the
    /// scriptless RawRequest probes), so the only refusal is an unknown id.
    func select(_ id: String) -> (ok: Bool, reason: String?) {
        guard let script = corpus[id] else {
            return (false, "unknown scenario id \(id)")
        }
        // Phase 4 WS: a2a-swift's WS layer is client-only this phase, so this
        // server cannot serve the WebSocket interface (ws-binding-v1 §1 /
        // WS-HARNESS.md). ws/* and transport-equivalence/* scenarios with
        // server=arkavo-ext-swift are honestly reported as a skip; the gate is
        // Swift-client → Rust-server (and the rust self-pair). The HTTP-only
        // legs of these scenarios cannot satisfy the WS binding's intent, so we
        // refuse rather than masquerade an HTTP pass as a WS pass.
        if id.hasPrefix("arkavo/ws/") || id.hasPrefix("arkavo/transport-equivalence/") {
            return (false, "Swift WS server not implemented (client-only this phase)")
        }
        // Phase 6 iroh discovery (iroh-discovery-v1 §7.4, IROH-HARNESS.md):
        // Swift has NO native iroh server (native iroh is Rust-first). Any
        // arkavo/discovery/* cell with server=arkavo-ext-swift is honestly
        // reported as a skip — the gate is rs↔rs native + swift-client→rust-
        // server-via-relay. (Same honest-skip pattern as the Swift WS server.)
        if id.hasPrefix("arkavo/discovery/") {
            return (false, "Swift has no iroh server; native iroh is Rust-first")
        }
        // arkavo-ext: scenario-keyed TDF arming (TDF-HARNESS.md). Inject the
        // encrypted part (+ sibling plaintext) into the scripted respond, write
        // the b3 blob payload-first, and serve the default (tdf-advertising)
        // card. The SDK path stays scenario-blind.
        if id.hasPrefix("arkavo/tdf/") {
            guard let rewritten = armTDF(script) else {
                return (false, "cannot arm tdf scenario \(id)")
            }
            var tdfScript = script
            tdfScript.respond = rewritten
            selected = tdfScript
            observedParams = nil
            cwtArmed = false
            gateArmed = false
            taskOpDenyArmed = false
            servedCard = defaultCard
            return (true, nil)
        }
        selected = script
        observedParams = nil
        // arkavo-ext: scenario-keyed identity arming (IDENTITY-HARNESS.md).
        cwtArmed = id.hasPrefix("arkavo/identity/cwt-")
        // arkavo-ext: scenario-keyed policy arming (POLICY-HARNESS.md). The
        // gate is armed ONLY while an arkavo/policy/* scenario is selected;
        // the TM-001 deny-all-task-ops rule only for gate-deny-task-op-32099.
        gateArmed = id.hasPrefix("arkavo/policy/")
        taskOpDenyArmed = id == "arkavo/policy/gate-deny-task-op-32099"
        if id == "arkavo/identity/card-signature-valid"
            || id == "arkavo/identity/card-signature-tampered"
        {
            guard let cardText = script.cardText else {
                return (false, "card-signature scenario has no server.card")
            }
            do {
                servedCard = try buildSignedCard(
                    cardText: cardText, tampered: id.hasSuffix("-tampered"))
            } catch {
                return (false, "cannot sign scenario card: \(error)")
            }
            return (true, nil)
        }
        if let cardText = script.cardText {
            let substituted = cardText.replacingOccurrences(
                of: "{{baseUrl}}", with: publicBaseUrl)
            let raw = Data(substituted.utf8)
            // Keep the SDK in the loop on the server side too: decode the
            // scripted card through the SDK and re-encode it. If the SDK
            // cannot round-trip the fixture, serve the raw bytes so the
            // client-side behavior is still exercised.
            if let card = try? A2AJSON.decoder().decode(AgentCard.self, from: raw),
                let encoded = try? A2AJSON.encoder().encode(card)
            {
                servedCard = encoded
            } else {
                servedCard = raw
            }
        } else {
            servedCard = defaultCard
        }
        return (true, nil)
    }

    func currentScript() -> ScenarioScript? {
        selected
    }

    func cardData() -> Data {
        servedCard
    }

    func record(params: Data) {
        observedParams = params
    }

    func observed() -> Data? {
        observedParams
    }

    /// GET /b3/<hex>: the payload-first ciphertext blob, or nil (404).
    func b3Blob(hex: String) -> Data? {
        b3Blobs[hex]
    }

    // MARK: - arkavo-ext: TDF parts arming (TDF-HARNESS.md)

    /// Inject the TDF artifact (+ sibling plaintext part) into the scripted
    /// respond JSON for an arkavo/tdf/* scenario, and (for b3) write the blob
    /// payload-first. Returns the rewritten respond Data, or nil on failure.
    private func armTDF(_ script: ScenarioScript) -> Data? {
        guard let respondData = script.respond,
            let respond = decodeJSON(respondData)
        else {
            return nil
        }
        let suffix = String(script.id.dropFirst("arkavo/tdf/".count))

        let parts: [Part]
        switch suffix {
        case "nanotdf-part-roundtrip", "tdf-part-wrong-key-fails-closed":
            // shape-(a): encrypt the known plaintext with the TEST dek (the
            // server always encrypts correctly; the wrong-key leg differs only
            // on the CLIENT's decrypt key).
            guard
                let enc = try? TDF.encryptInline(
                    plaintext: Data(TDFConstants.knownPlaintext.utf8),
                    dek: tdf.testDEK,
                    nonce: TDFConstants.nonce,
                    kasUrl: TDFConstants.kasUrl)
            else { return nil }
            parts = [enc, Part.text(TDFConstants.siblingPlaintext)]
        case "b3-url-artifact-integrity":
            // shape-(b): encrypt, take the inline archive (manifest+ciphertext)
            // JSON as the blob, write it payload-first, reference it by BLAKE3.
            guard
                let enc = try? TDF.encryptInline(
                    plaintext: Data(TDFConstants.knownPlaintext.utf8),
                    dek: tdf.testDEK,
                    nonce: TDFConstants.nonce,
                    kasUrl: TDFConstants.kasUrl),
                case .data(let inlineValue) = enc.content
            else { return nil }
            let blob = encodeJSON(inlineValue)
            let b3hex = blake3Hex(blob)
            // Payload-first: write the blob BEFORE the message is served.
            b3Blobs[b3hex] = blob
            // The b3 URL uses the PUBLIC base (the capture proxy) so the client
            // fetches through the proxy; the /b3/<hex> path shape is pinned.
            let manifestSidecar = inlineValue.objectValue?["manifest"]
            let urlPart = makeB3URLPart(
                blobHex: b3hex, gateway: publicBaseUrl, tdfManifest: manifestSidecar)
            parts = [urlPart, Part.text(TDFConstants.siblingPlaintext)]
        default:
            return nil
        }

        let artifact = Artifact(
            artifactId: "art-tdf-1",
            name: "tdf-artifact",
            parts: parts,
            extensions: [TDFExtension.uri])

        // Inject the artifact into the scripted task/message JSON.
        guard var members = respond.objectValue else { return nil }
        let artifactValue = (try? encodeToJSONValueServer(artifact)) ?? .null
        if var taskMembers = members["task"]?.objectValue {
            // SendMessage -> {"task": {...}}
            var artifacts = taskMembers["artifacts"]?.arrayValue ?? []
            artifacts.append(artifactValue)
            taskMembers["artifacts"] = .array(artifacts)
            members["task"] = .object(taskMembers)
        } else {
            // GetTask -> {...task...}
            var artifacts = members["artifacts"]?.arrayValue ?? []
            artifacts.append(artifactValue)
            members["artifacts"] = .array(artifacts)
        }
        return encodeJSON(.object(members))
    }
}

/// Encode any Encodable to a JSONValue (server-side helper).
func encodeToJSONValueServer<Value: Encodable>(_ value: Value) throws -> JSONValue {
    let data = try A2AJSON.encoder().encode(value)
    return try A2AJSON.decoder().decode(JSONValue.self, from: data)
}
