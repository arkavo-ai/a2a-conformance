// arkavo-ext-swift server harness (a2a-conformance adapter contract v1).
//
//   server-harness --scenarios <dir> --public-base-url <URL>
//
// The core adapters/arkavo-swift server harness, extended with the arkavo
// identity layer (ArkavoA2AIdentity): while an `arkavo/identity/cwt-*`
// scenario is selected, the POST route enforces Bearer-CWT auth via
// CwtVerifier before requests reach the SDK dispatcher (401 per spec §8);
// the card-signature scenarios serve a DID-signed (or post-signing tampered)
// card; the default card advertises the aia-identity extension. Every other
// scenario behaves byte-identically to adapters/arkavo-swift.
//
// Binds two ephemeral ports: the A2A port (a2a-swift's JSONRPCDispatcher
// served over Hummingbird) and a control port (plain HTTP: POST /select,
// GET /observed). Prints exactly one READY line to stdout; all logs go to
// stderr. Exits on stdin EOF.

import A2A
import A2AServer
import ArkavoA2AIdentity
import Foundation
import Hummingbird
import Logging
import NIOCore

// MARK: - Arguments

var scenariosDir: String?
var publicBaseUrl: String?
var argumentIterator = CommandLine.arguments.dropFirst().makeIterator()
while let argument = argumentIterator.next() {
    switch argument {
    case "--scenarios":
        scenariosDir = argumentIterator.next()
    case "--public-base-url":
        publicBaseUrl = argumentIterator.next()
    default:
        fail("unknown argument \(argument)")
    }
}
guard let scenariosDir, let publicBaseUrl else {
    fail("usage: server-harness --scenarios <dir> --public-base-url <URL>")
}

let corpus: [String: ScenarioScript]
do {
    corpus = try ScenarioCorpus.load(from: scenariosDir)
} catch {
    fail("\(error)")
}
let identity: IdentityFixtures
do {
    identity = try IdentityFixtures.load()
} catch {
    fail("identity fixtures unavailable: \(error)")
}
// One verifier for the whole process: the §5 cti replay cache must be
// process-scoped, not per-scenario.
let cwtVerifier = CwtVerifier(
    configuration: CwtVerifierConfiguration(
        trustedIssuerKey: identity.issuerPublicKey,
        expectedIssuer: identity.iss,
        expectedAudience: identity.serverDid))
let state = ScenarioState(corpus: corpus, publicBaseUrl: publicBaseUrl, identity: identity)
// arkavo-ext: while an arkavo/policy/* scenario is selected, the routing
// handler dispatches through ArkavoA2APolicy's GatedRequestHandler over the
// scripted handler (POLICY-HARNESS.md); otherwise the plain scripted path.
let dispatcher = JSONRPCDispatcher(
    handler: PolicyRoutingHandler(state: state, scripted: ScriptedHandler(state: state)))

// Stdout is reserved for the single READY line; route all logging to stderr.
var logger = Logger(
    label: "arkavo-server-harness",
    factory: { StreamLogHandler.standardError(label: $0) })
logger.logLevel = .error

// MARK: - A2A application (the SDK under test)

let a2aRouter = Router()

a2aRouter.get(RouterPath(A2AProtocol.agentCardWellKnownPath)) { _, _ in
    jsonResponse(await state.cardData())
}

a2aRouter.post(RouterPath("/")) { request, _ -> Response in
    // arkavo-ext: Bearer-CWT enforcement, armed per scenario state. The
    // card GET above stays anonymous in both states (peers must read the
    // extension advertisement to learn the aud, spec §1). A rejected
    // request never reaches the SDK's JSON-RPC dispatcher (spec §8).
    if await state.authArmed(),
        let rejection = await authenticateBearerCWT(request, verifier: cwtVerifier)
    {
        return rejection
    }

    let body = try await request.body.collect(upTo: 16 << 20)
    let bodyData = Data(body.readableBytesView)

    // Raw-result fixture branch (e.g. edge/unknown-field-tolerance): those
    // scenarios test the *client's* decode tolerance, and the typed dispatcher
    // cannot emit arbitrary JSON, so the harness writes the JSON-RPC envelope
    // itself: {"jsonrpc":"2.0","id":<echoed>,"result":<rawResult>}.
    if let rawResult = await state.currentScript()?.rawResult {
        let requestObject =
            (try? JSONSerialization.jsonObject(with: bodyData)) as? [String: Any]
        let envelope: [String: Any] = [
            "jsonrpc": "2.0",
            "id": requestObject?["id"] ?? NSNull(),
            "result": try JSONSerialization.jsonObject(
                with: rawResult, options: [.fragmentsAllowed]),
        ]
        return jsonResponse(try JSONSerialization.data(withJSONObject: envelope))
    }

    switch await dispatcher.dispatch(bodyData) {
    case .single(let data):
        return jsonResponse(data)
    case .stream(let events):
        return Response(
            status: .ok,
            headers: [.contentType: "text/event-stream"],
            body: ResponseBody(asyncSequence: events.map { ByteBuffer(bytes: $0) }))
    }
}

// MARK: - Control application (harness-owned, never the SDK)

let controlRouter = Router()

controlRouter.post(RouterPath("/select")) { request, _ -> Response in
    let body = try await request.body.collect(upTo: 1 << 20)
    guard
        let object = (try? JSONSerialization.jsonObject(with: Data(body.readableBytesView)))
            as? [String: Any],
        let scenarioID = object["scenario"] as? String
    else {
        return jsonResponse(
            try JSONSerialization.data(withJSONObject: [
                "ok": false, "reason": "malformed /select body; expected {\"scenario\": id}",
            ]))
    }
    let (ok, reason) = await state.select(scenarioID)
    var reply: [String: Any] = ["ok": ok]
    if let reason {
        reply["reason"] = reason
    }
    return jsonResponse(try JSONSerialization.data(withJSONObject: reply))
}

controlRouter.get(RouterPath("/observed")) { _, _ -> Response in
    if let params = await state.observed() {
        var body = Data("{\"params\": ".utf8)
        body.append(params)
        body.append(Data("}".utf8))
        return jsonResponse(body)
    }
    return jsonResponse(Data("{\"params\": null}".utf8))
}

// MARK: - Run both servers on ephemeral ports, then announce READY

let (portEvents, portContinuation) = AsyncStream.makeStream(of: PortEvent.self)

let a2aApp = Application(
    router: a2aRouter,
    configuration: .init(address: .hostname("127.0.0.1", port: 0)),
    onServerRunning: { channel in
        portContinuation.yield(.a2a(channel.localAddress?.port ?? 0))
    },
    logger: logger)

let controlApp = Application(
    router: controlRouter,
    configuration: .init(address: .hostname("127.0.0.1", port: 0)),
    onServerRunning: { channel in
        portContinuation.yield(.control(channel.localAddress?.port ?? 0))
    },
    logger: logger)

// Exit on stdin EOF (contract §"Server harness" step 6).
Thread.detachNewThread {
    while readLine(strippingNewline: false) != nil {}
    exit(0)
}

try await withThrowingTaskGroup(of: Void.self) { group in
    group.addTask { try await a2aApp.runService() }
    group.addTask { try await controlApp.runService() }
    group.addTask {
        var a2aPort: Int?
        var controlPort: Int?
        for await event in portEvents {
            switch event {
            case .a2a(let port): a2aPort = port
            case .control(let port): controlPort = port
            }
            if let a2aPort, let controlPort {
                let line =
                    "READY {\"port\": \(a2aPort), \"controlPort\": \(controlPort), "
                    + "\"baseUrl\": \"http://127.0.0.1:\(a2aPort)\"}\n"
                // FileHandle writes are unbuffered syscalls: no fflush needed.
                // (fflush(nil) deadlocks against the stdin-EOF watcher thread,
                // which holds stdin's stream lock while blocked in readLine;
                // referencing the stdout global is a strict-concurrency error
                // on Linux.)
                FileHandle.standardOutput.write(Data(line.utf8))
                break
            }
        }
    }
    // The servers run until the process exits (stdin EOF -> exit(0) above);
    // waiting on all children keeps both applications alive and propagates
    // any startup failure (e.g. bind error).
    try await group.waitForAll()
}
