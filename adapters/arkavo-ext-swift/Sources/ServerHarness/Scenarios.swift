// Scenario corpus loading and the per-run mutable state (selected scenario,
// served card, observation buffer). Scenario JSON is parsed with
// JSONSerialization; only `id`, `client.op`, and the `server` section matter
// to the server harness. Scripted payloads are kept as raw JSON `Data` and
// decoded into SDK types at request time so the SDK's codecs stay in the loop.

import A2A
import ArkavoA2AIdentity
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
    private let defaultCard: Data
    private var selected: ScenarioScript?
    private var servedCard: Data
    private var observedParams: Data?
    /// True while an `arkavo/identity/cwt-*` scenario is selected (the POST
    /// route then enforces Bearer-CWT auth before dispatching to the SDK).
    private var cwtArmed = false

    init(corpus: [String: ScenarioScript], publicBaseUrl: String, identity: IdentityFixtures) {
        self.corpus = corpus
        self.publicBaseUrl = publicBaseUrl
        self.identity = identity
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
                        ])
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
        selected = script
        observedParams = nil
        // arkavo-ext: scenario-keyed identity arming (IDENTITY-HARNESS.md).
        cwtArmed = id.hasPrefix("arkavo/identity/cwt-")
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
}
