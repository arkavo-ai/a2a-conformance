// ServerHarnessMain.swift
// ServerHarness — conformance server harness wrapping tolgaki's
// a2a-swift-server (third party, under test).
//
// Usage: ServerHarness --scenarios <dir> --public-base-url <URL>
//
// Binds two ephemeral ports:
//   - A2A port: served entirely by the SDK's own Hummingbird router
//     (envelope parsing, method routing, decode/encode, error mapping,
//     SSE framing are all the SDK's).
//   - control port: harness-owned Hummingbird app (POST /select,
//     GET /observed). Never the SDK.
//
// Prints exactly one READY line to stdout; all logs go to stderr
// (swift-log is bootstrapped onto stderr before anything else runs,
// because the SDK logs through swift-log which defaults to stdout).

import Foundation
import A2AServer
import Hummingbird
import Logging
import NIOCore
import HarnessSupport

// MARK: - Free port picking
//
// A2AServer.bind("host:port") parses a literal port (port 0 is passed through
// to Hummingbird but the bound port would not be discoverable through the
// SDK's public API), so the harness picks free ports by binding/releasing a
// socket first. Small race window, acceptable for a test harness.

func pickFreePort() throws -> Int {
    let fd = socket(AF_INET, SOCK_STREAM, 0)
    guard fd >= 0 else { throw HarnessError("socket() failed: errno \(errno)") }
    defer { close(fd) }
    var addr = sockaddr_in()
    addr.sin_family = sa_family_t(AF_INET)
    addr.sin_port = 0
    addr.sin_addr.s_addr = inet_addr("127.0.0.1")
    var one: Int32 = 1
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, socklen_t(MemoryLayout<Int32>.size))
    let bindResult = withUnsafePointer(to: &addr) {
        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            Darwin.bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
        }
    }
    guard bindResult == 0 else { throw HarnessError("bind() failed: errno \(errno)") }
    var bound = sockaddr_in()
    var len = socklen_t(MemoryLayout<sockaddr_in>.size)
    let nameResult = withUnsafeMutablePointer(to: &bound) {
        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            getsockname(fd, $0, &len)
        }
    }
    guard nameResult == 0 else { throw HarnessError("getsockname() failed: errno \(errno)") }
    return Int(UInt16(bigEndian: bound.sin_port))
}

struct HarnessError: Error, CustomStringConvertible {
    let description: String
    init(_ d: String) { description = d }
}

// MARK: - Task cleanup tracking

final class TaskJanitor: @unchecked Sendable {
    private let lock = NSLock()
    private var ids: Set<String> = []

    func add(_ id: String) {
        lock.lock(); defer { lock.unlock() }
        ids.insert(id)
    }

    func takeAll() -> Set<String> {
        lock.lock(); defer { lock.unlock() }
        let all = ids
        ids = []
        return all
    }
}

// MARK: - Main

@main
struct ServerHarnessMain {
    static func main() async throws {
        // FIRST: route all swift-log output (including the SDK's) to stderr
        // so stdout carries only the READY line.
        LoggingSystem.bootstrap { label in
            var handler = StreamLogHandler.standardError(label: label)
            handler.logLevel = .warning
            return handler
        }

        // --- Arguments
        var scenariosDir: String?
        var publicBaseUrl: String?
        var args = ArraySlice(CommandLine.arguments.dropFirst())
        while let arg = args.popFirst() {
            switch arg {
            case "--scenarios": scenariosDir = args.popFirst()
            case "--public-base-url": publicBaseUrl = args.popFirst()
            default: HarnessIO.log("ignoring unknown argument \(arg)")
            }
        }
        guard let scenariosDir, let publicBaseUrl else {
            HarnessIO.log("usage: ServerHarness --scenarios <dir> --public-base-url <URL>")
            exit(2)
        }

        let scenarios = loadScenarios(from: scenariosDir)
        HarnessIO.log("loaded \(scenarios.count) scenarios from \(scenariosDir)")

        // --- Default agent card (used when a scenario scripts no card).
        let defaultCardJSON: JSONValue = .object([
            "name": .string("Tolgaki Conformance Harness"),
            "description": .string("Scripted conformance agent backed by tolgaki/a2a-swift-server."),
            "version": .string("1.0.0"),
            "supportedInterfaces": .array([.object([
                "url": .string(publicBaseUrl),
                "protocolBinding": .string("JSONRPC"),
                "protocolVersion": .string("1.0"),
            ])]),
            "capabilities": .object(["streaming": .bool(true)]),
            "defaultInputModes": .array([.string("text/plain")]),
            "defaultOutputModes": .array([.string("text/plain")]),
            "skills": .array([.object([
                "id": .string("conformance"),
                "name": .string("Conformance"),
                "description": .string("Scripted conformance agent."),
                "tags": .array([.string("conformance")]),
            ])]),
        ])
        let defaultCard = try decodeServerSDK(AgentCard.self, from: defaultCardJSON)

        let state = ScriptState(defaultCard: defaultCard)
        let janitor = TaskJanitor()
        let taskStore = InMemoryTaskStore()

        // --- Ports
        let a2aPort = try pickFreePort()
        var pickedControlPort = try pickFreePort()
        while pickedControlPort == a2aPort { pickedControlPort = try pickFreePort() }
        let controlPort = pickedControlPort

        // --- The SDK's server (the thing under test).
        let sdkServer = A2AServer(
            handler: ScriptedHandler(state: state),
            taskStore: taskStore
        ).bind("127.0.0.1:\(a2aPort)")

        // --- Control endpoint (harness-owned).
        let router = Router()
        router.post("/select") { request, _ -> Response in
            let buffer = try await request.body.collect(upTo: 1 << 20)
            let body = (try? JSONValue.parse(Data(buffer.readableBytesView))) ?? .null
            let result = await handleSelect(
                body: body,
                scenarios: scenarios,
                state: state,
                janitor: janitor,
                taskStore: taskStore,
                a2aPort: a2aPort,
                publicBaseUrl: publicBaseUrl
            )
            return try controlJSONResponse(result)
        }
        router.get("/observed") { _, _ -> Response in
            try controlJSONResponse(.object(["params": state.observedParams]))
        }
        let controlApp = Application(
            router: router,
            configuration: .init(address: .hostname("127.0.0.1", port: controlPort), serverName: "conformance-control")
        )

        // --- stdin EOF -> exit (contract).
        Thread.detachNewThread {
            while readLine(strippingNewline: false) != nil {}
            HarnessIO.log("stdin EOF — server harness exiting")
            exit(0)
        }

        // --- Run both servers; print READY once both accept connections.
        await withThrowingTaskGroup(of: Void.self) { group in
            group.addTask { try await sdkServer.run() }
            group.addTask { try await controlApp.runService() }
            group.addTask {
                try await waitUntilAccepting(port: a2aPort)
                try await waitUntilAccepting(port: controlPort)
                let ready: JSONValue = .object([
                    "port": .int(a2aPort),
                    "controlPort": .int(controlPort),
                    "baseUrl": .string("http://127.0.0.1:\(a2aPort)"),
                ])
                HarnessIO.emitLine("READY " + (try ready.serializedString()))
                HarnessIO.log("ready: a2a=\(a2aPort) control=\(controlPort) publicBaseUrl=\(publicBaseUrl)")
            }
            // If either server task ever returns/throws, the harness is dead.
            do {
                try await group.next()
                try await group.next()
                try await group.next()
            } catch {
                HarnessIO.log("FATAL: \(error)")
                exit(1)
            }
        }
    }
}

// MARK: - Control responses

func controlJSONResponse(_ value: JSONValue) throws -> Response {
    let data = try value.serialized()
    var headers = HTTPFields()
    headers[.contentType] = "application/json"
    return Response(status: .ok, headers: headers, body: .init(byteBuffer: ByteBuffer(bytes: data)))
}

// MARK: - Readiness probe

func waitUntilAccepting(port: Int, attempts: Int = 200) async throws {
    for _ in 0..<attempts {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        if fd >= 0 {
            var addr = sockaddr_in()
            addr.sin_family = sa_family_t(AF_INET)
            addr.sin_port = in_port_t(UInt16(port).bigEndian)
            addr.sin_addr.s_addr = inet_addr("127.0.0.1")
            let result = withUnsafePointer(to: &addr) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    Darwin.connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
                }
            }
            close(fd)
            if result == 0 { return }
        }
        try await Task.sleep(nanoseconds: 50_000_000)
    }
    throw HarnessError("port \(port) never started accepting connections")
}

// MARK: - /select

func handleSelect(
    body: JSONValue,
    scenarios: [String: Scenario],
    state: ScriptState,
    janitor: TaskJanitor,
    taskStore: InMemoryTaskStore,
    a2aPort: Int,
    publicBaseUrl: String
) async -> JSONValue {
    func no(_ reason: String) -> JSONValue {
        HarnessIO.log("/select -> ok:false (\(reason))")
        state.arm(nil, card: nil)
        return .object(["ok": .bool(false), "reason": .string(reason)])
    }

    guard let id = body["scenario"]?.stringValue else {
        return no("select body missing scenario id")
    }
    guard let scenario = scenarios[id] else {
        return no("unknown scenario id \(id)")
    }
    if let reason = evaluateSupport(scenario) {
        return no(reason)
    }

    // Card: scenario card with {{baseUrl}} -> --public-base-url, decoded
    // into the SDK's AgentCard (the SDK serves its own re-encoding of it).
    var card: AgentCard?
    if let cardJSON = scenario.card {
        do {
            let substituted = try cardJSON.serializedString()
                .replacingOccurrences(of: "{{baseUrl}}", with: publicBaseUrl)
            card = try sdkServerDecoder().decode(AgentCard.self, from: Data(substituted.utf8))
        } catch {
            return no("SDK AgentCard cannot represent the scenario card: \(error)")
        }
    }

    // Clean tasks left in the SDK's store by previous scenarios.
    for tid in janitor.takeAll() {
        await taskStore.delete(id: tid)
    }

    // Arm the scripted handler.
    var armed = ArmedScenario(id: id)
    do {
        if let code = scenario.errorCode {
            switch (scenario.op, code) {
            case ("SendMessage", _):
                guard let err = throwableError(code: code, message: scenario.errorMessage) else {
                    return no("error \(code) has no throwable mapping") // unreachable; evaluateSupport gates
                }
                armed.thrownError = err
            case ("GetExtendedAgentCard", -32007):
                armed.extendedCardNotConfigured = true
            case ("GetTask", -32001), ("CancelTask", -32002):
                break // natural framework behavior (CancelTask needs the seed below)
            default:
                return no("error \(code) for op \(scenario.op) cannot be scripted") // unreachable
            }
        } else {
            switch scenario.op {
            case "SendMessage":
                guard let respond = scenario.respond else { return no("missing respond") }
                let response = try decodeServerSDK(SendMessageResponse.self, from: respond)
                armed.sendResponse = response
                if case .task(let t) = response { janitor.add(t.id) } // framework inserts it on send
            case "SendStreamingMessage", "SubscribeToTask":
                guard let sse = scenario.sse else { return no("missing sse") }
                let frames = try sse.map { try decodeServerSDK(StreamResponse.self, from: $0) }
                armed.sseFrames = frames
                for frame in frames {
                    if case .task(let t) = frame { janitor.add(t.id) } // framework persists snapshots
                }
            default:
                break
            }
        }
    } catch {
        return no("SDK types cannot decode the scripted server section: \(error)")
    }

    state.arm(armed, card: card)

    // Seed the SDK's task store where the scenario depends on pre-existing
    // task state, via a loopback SendMessage through the SDK's own endpoint.
    if let (seed, derivation) = computeSeed(scenario) {
        do {
            let seedTask = try seed()
            state.setPendingSeed(.task(seedTask))
            if let failure = await runLoopbackSeed(expected: seedTask, a2aPort: a2aPort, taskStore: taskStore) {
                return no(failure)
            }
            janitor.add(seedTask.id)
            HarnessIO.log("seeded task \(seedTask.id) (\(derivation))")
        } catch {
            return no("SDK types cannot decode the seed task: \(error)")
        }
    }

    state.resetObserved()
    HarnessIO.log("/select -> ok:true (\(id))")
    return .object(["ok": .bool(true)])
}

/// Returns a closure producing the seed task plus a human-readable note on
/// how the seed was derived, or nil when the scenario needs no seed.
func computeSeed(_ scenario: Scenario) -> ((() throws -> A2ATask), String)? {
    switch (scenario.op, scenario.errorCode) {
    case ("GetTask", nil):
        guard let respond = scenario.respond else { return nil }
        return ({ try decodeServerSDK(A2ATask.self, from: respond) },
                "scenario respond task, verbatim")
    case ("CancelTask", nil):
        guard let respond = scenario.respond else { return nil }
        return ({
            let target = try decodeServerSDK(A2ATask.self, from: respond)
            // Precondition derivation: the scripted respond is the POST-cancel
            // task; the framework owns the cancel transition, so the seed is
            // the same task in a non-terminal state. The framework computes
            // the canceled state/timestamp itself.
            return A2ATask(
                id: target.id,
                contextId: target.contextId,
                status: TaskStatus(state: .working),
                artifacts: target.artifacts,
                history: target.history,
                metadata: target.metadata
            )
        }, "respond task reset to TASK_STATE_WORKING so the SDK performs the cancel transition itself")
    case ("CancelTask", .some(-32002)):
        let id = scenario.clientParams?["id"]?.stringValue ?? "t-conformance-terminal"
        return ({
            A2ATask(
                id: id,
                contextId: "c-conformance-seed",
                status: TaskStatus(state: .completed)
            )
        }, "synthetic terminal task so the SDK's own not-cancelable check fires")
    default:
        return nil
    }
}

/// POSTs a SendMessage to the SDK's own A2A endpoint whose scripted response
/// is the seed task, then verifies the SDK stored it unmodified.
func runLoopbackSeed(expected: A2ATask, a2aPort: Int, taskStore: InMemoryTaskStore) async -> String? {
    let rpc: JSONValue = .object([
        "jsonrpc": .string("2.0"),
        "id": .string("conformance-seed"),
        "method": .string("SendMessage"),
        "params": .object([
            "message": .object([
                "messageId": .string("conformance-seed"),
                "role": .string("ROLE_USER"),
                "parts": .array([.object(["text": .string("seed")])]),
            ]),
        ]),
    ])
    do {
        var request = URLRequest(url: URL(string: "http://127.0.0.1:\(a2aPort)/")!)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try rpc.serialized()
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            return "loopback seed SendMessage failed: HTTP \((response as? HTTPURLResponse)?.statusCode ?? -1)"
        }
        let envelope = try JSONValue.parse(data)
        if let err = envelope["error"], !err.isNull {
            return "loopback seed SendMessage returned error: \(try err.serializedString())"
        }
    } catch {
        return "loopback seed SendMessage failed: \(error)"
    }

    // Verify the SDK stored the task under the scripted id, unmodified.
    guard let stored = await taskStore.get(id: expected.id) else {
        return "SDK framework did not store the seeded task under id \(expected.id); cannot serve this scenario from its task store"
    }
    if stored.status.state != expected.status.state {
        return "SDK framework rewrote the seeded task state (\(expected.status.state.rawValue) -> \(stored.status.state.rawValue))"
    }
    if stored.contextId != expected.contextId {
        return "SDK framework rewrote the seeded task contextId (\(expected.contextId) -> \(stored.contextId))"
    }
    if (stored.history?.count ?? 0) != (expected.history?.count ?? 0) {
        return "SDK framework altered the seeded task history (\(expected.history?.count ?? 0) -> \(stored.history?.count ?? 0) messages)"
    }
    if (stored.artifacts?.count ?? 0) != (expected.artifacts?.count ?? 0) {
        return "SDK framework altered the seeded task artifacts"
    }
    return nil
}
