// arkavo-swift server harness (a2a-conformance adapter contract v1).
//
//   server-harness --scenarios <dir> --public-base-url <URL>
//
// Binds two ephemeral ports: the A2A port (a2a-swift's JSONRPCDispatcher
// served over Hummingbird) and a control port (plain HTTP: POST /select,
// GET /observed). Prints exactly one READY line to stdout; all logs go to
// stderr. Exits on stdin EOF.

import A2A
import A2AServer
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
let state = ScenarioState(corpus: corpus, publicBaseUrl: publicBaseUrl)
let dispatcher = JSONRPCDispatcher(handler: ScriptedHandler(state: state))

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
                    + "\"baseUrl\": \"http://127.0.0.1:\(a2aPort)\"}"
                print(line)
                fflush(stdout)
                break
            }
        }
    }
    // The servers run until the process exits (stdin EOF -> exit(0) above);
    // waiting on all children keeps both applications alive and propagates
    // any startup failure (e.g. bind error).
    try await group.waitForAll()
}
