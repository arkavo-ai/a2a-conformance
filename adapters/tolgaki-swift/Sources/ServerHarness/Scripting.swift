// Scripting.swift
// ServerHarness — scripted A2AHandler state for tolgaki's a2a-swift-server.
//
// Upstream server SDK: https://github.com/tolgaki/a2a-swift-server
//   pinned SHA 0a1db4f759250a23ac6ca9528ca76d5f0ae8ea64
//
// Fairness rules honored here:
//  - The SDK's own Hummingbird router/dispatcher serves the A2A port; the
//    harness only plugs in at the public A2AHandler + TaskStore/Authenticator
//    extension points.
//  - GetTask/ListTasks/CancelTask/SubscribeToTask/push-config ops are owned
//    by the SDK framework (its task store + error mapping). Where the
//    framework cannot be made to produce a scenario's scripted behavior
//    through those public extension points, the scenario is reported
//    unsupported at /select — never approximated.
//  - Task-store seeding happens through a loopback SendMessage POSTed to the
//    SDK's own A2A endpoint (so the SDK's insertion path runs), and the
//    stored task is verified afterwards.

import Foundation
import A2AServer // @_exported imports A2AClient (a2a-client-swift 1.0.22)
import HarnessSupport

// MARK: - Scenario model

struct Scenario: Sendable {
    let id: String
    let op: String
    let clientParams: JSONValue?
    let respond: JSONValue?
    let errorCode: Int?
    let errorMessage: String?
    let sse: [JSONValue]?
    let card: JSONValue?
    let rawResult: String?
    let expectRequestPaths: [String]

    init?(json: JSONValue) {
        guard let id = json["id"]?.stringValue,
              let op = json["client"]?["op"]?.stringValue else { return nil }
        self.id = id
        self.op = op
        self.clientParams = json["client"]?["params"]
        let server = json["server"]
        self.respond = server?["respond"]
        self.errorCode = server?["error"]?["code"]?.intValue
        self.errorMessage = server?["error"]?["message"]?.stringValue
        self.sse = server?["sse"]?.arrayValue
        self.card = server?["card"]
        self.rawResult = server?["rawResult"]?.stringValue
        self.expectRequestPaths = json["expectRequest"]?["checks"]?.arrayValue?
            .compactMap { $0["path"]?.stringValue } ?? []
    }
}

func loadScenarios(from dir: String) -> [String: Scenario] {
    var result: [String: Scenario] = [:]
    let fm = FileManager.default
    guard let enumerator = fm.enumerator(atPath: dir) else {
        HarnessIO.log("WARNING: cannot enumerate scenarios dir \(dir)")
        return result
    }
    for case let path as String in enumerator where path.hasSuffix(".json") {
        let full = (dir as NSString).appendingPathComponent(path)
        guard let data = fm.contents(atPath: full),
              let json = try? JSONValue.parse(data),
              let scenario = Scenario(json: json) else {
            HarnessIO.log("WARNING: skipping unparsable scenario file \(full)")
            continue
        }
        result[scenario.id] = scenario
    }
    return result
}

// MARK: - SDK codec helpers (match the SDK server's encoder/decoder config)

func sdkServerDecoder() -> JSONDecoder {
    let d = JSONDecoder()
    d.dateDecodingStrategy = .iso8601
    return d
}

func sdkServerEncoder() -> JSONEncoder {
    let e = JSONEncoder()
    e.dateEncodingStrategy = .iso8601
    e.outputFormatting = [.sortedKeys]
    return e
}

func decodeServerSDK<T: Decodable>(_ type: T.Type, from json: JSONValue) throws -> T {
    try sdkServerDecoder().decode(type, from: json.serialized(sortedKeys: false))
}

func encodeServerSDK<T: Encodable>(_ value: T) throws -> JSONValue {
    try JSONValue.parse(sdkServerEncoder().encode(value))
}

// MARK: - Armed (decoded) scenario state

/// Everything the scripted handler needs for the currently selected scenario,
/// pre-decoded into the SDK's types at /select time so decode failures are
/// reported as `ok:false` instead of mid-scenario surprises.
struct ArmedScenario: Sendable {
    let id: String
    var sendResponse: SendMessageResponse?
    var thrownError: A2AError?
    var sseFrames: [StreamResponse]?
    var extendedCardNotConfigured: Bool = false

    init(id: String) { self.id = id }
}

final class ScriptState: @unchecked Sendable {
    private let lock = NSLock()
    private var armed: ArmedScenario?
    private var card: AgentCard?
    private var observed: JSONValue?
    private var pendingSeed: SendMessageResponse?

    let defaultCard: AgentCard

    init(defaultCard: AgentCard) {
        self.defaultCard = defaultCard
    }

    func arm(_ scenario: ArmedScenario?, card: AgentCard?) {
        lock.lock(); defer { lock.unlock() }
        self.armed = scenario
        self.card = card
        self.observed = nil
    }

    var current: ArmedScenario? {
        lock.lock(); defer { lock.unlock() }
        return armed
    }

    var currentCard: AgentCard {
        lock.lock(); defer { lock.unlock() }
        return card ?? defaultCard
    }

    func setPendingSeed(_ seed: SendMessageResponse) {
        lock.lock(); defer { lock.unlock() }
        pendingSeed = seed
    }

    func takePendingSeed() -> SendMessageResponse? {
        lock.lock(); defer { lock.unlock() }
        let s = pendingSeed
        pendingSeed = nil
        return s
    }

    func recordObserved(_ value: JSONValue) {
        lock.lock(); defer { lock.unlock() }
        observed = value
    }

    func resetObserved() {
        lock.lock(); defer { lock.unlock() }
        observed = nil
    }

    var observedParams: JSONValue {
        lock.lock(); defer { lock.unlock() }
        return observed ?? .null
    }
}

// MARK: - Scripted handler

/// The handler only ever sees the decoded `Message` (+auth) — that is the
/// full extent of what this SDK's handler abstraction exposes, and therefore
/// the full extent of what /observed can report.
struct ScriptedHandler: A2AHandler {
    let state: ScriptState

    func handleMessage(_ message: Message, auth: AuthContext?) async throws -> SendMessageResponse {
        // Loopback seeding request from the harness itself (never observed).
        if let seed = state.takePendingSeed() {
            return seed
        }

        recordMessage(message)

        guard let armed = state.current else {
            throw A2AError.internalError(message: "conformance harness: no scenario selected")
        }
        if let error = armed.thrownError {
            throw error
        }
        if let response = armed.sendResponse {
            return response
        }
        throw A2AError.internalError(message: "conformance harness: scenario \(armed.id) has no scripted SendMessage response")
    }

    func handleStreamingMessage(_ message: Message, auth: AuthContext?) -> AsyncThrowingStream<StreamResponse, Error> {
        recordMessage(message)
        let armed = state.current
        return AsyncThrowingStream { continuation in
            guard let armed else {
                continuation.finish(throwing: A2AError.internalError(message: "conformance harness: no scenario selected"))
                return
            }
            if let error = armed.thrownError {
                continuation.finish(throwing: error)
                return
            }
            guard let frames = armed.sseFrames else {
                continuation.finish(throwing: A2AError.internalError(message: "conformance harness: scenario \(armed.id) has no scripted SSE frames"))
                return
            }
            for frame in frames {
                continuation.yield(frame)
            }
            continuation.finish()
        }
    }

    func agentCard(baseURL: String) -> AgentCard {
        // The scenario card (with {{baseUrl}} already substituted with
        // --public-base-url) wins over the Host-derived baseURL.
        state.currentCard
    }

    func extendedAgentCard(baseURL: String, auth: AuthContext) async throws -> AgentCard? {
        if state.current?.extendedCardNotConfigured == true {
            // Returning nil makes the SDK framework itself throw
            // extendedAgentCardNotConfigured (-32007).
            return nil
        }
        return state.currentCard
    }

    private func recordMessage(_ message: Message) {
        // /observed contract: the params as the SDK decoded + re-encoded
        // them. This SDK hands the handler only the Message, so that is all
        // that can be reported (tenant/configuration/metadata are consumed
        // by the framework and not exposed).
        if let encoded = try? encodeServerSDK(message) {
            state.recordObserved(.object(["message": encoded]))
        } else {
            state.recordObserved(.null)
        }
    }
}

// MARK: - Scripted error mapping (SendMessage path only)

/// Maps a scripted error code onto the SDK's semantic error case so the
/// SDK framework (JSONRPC.swift toJSONRPCError) performs the code mapping.
/// Only codes with a semantic case reachable from handleMessage are mapped;
/// everything else must be reported unsupported by evaluateSupport.
func throwableError(code: Int, message: String?) -> A2AError? {
    switch code {
    case -32005: return .contentTypeNotSupported(contentType: "", message: message)
    case -32006: return .invalidAgentResponse(message: message)
    case -32008: return .extensionSupportRequired(extensionUri: "", message: message)
    case -32009: return .versionNotSupported(version: "", supportedVersions: nil, message: message)
    default: return nil
    }
}

// MARK: - Support evaluation

/// Returns nil when the scenario can be served honestly through this SDK's
/// extension points, else a human-readable reason for /select ok:false.
func evaluateSupport(_ s: Scenario) -> String? {
    // 1. Request observation limits: the A2AHandler only receives the
    //    decoded Message, so only $.params.message... is observable.
    if !s.expectRequestPaths.isEmpty {
        let observableOps = ["SendMessage", "SendStreamingMessage"]
        let unobservable = s.expectRequestPaths.filter { !$0.hasPrefix("$.params.message") }
        if !observableOps.contains(s.op) || !unobservable.isEmpty {
            return "expectRequest paths \(s.expectRequestPaths) are not observable: this SDK's A2AHandler exposes only the decoded Message to user code (tenant/historyLength/etc. are consumed by the framework)"
        }
    }

    // 2. rawResult scripting is impossible: framework-owned ops re-encode
    //    through typed models, which would silently drop the unknown fields
    //    the scenario exists to test.
    if s.rawResult != nil {
        return "server.rawResult cannot be emitted verbatim: \(s.op) is framework-owned and re-encodes through typed A2ATask, which drops unknown fields (serving a stripped result would fake the scenario)"
    }

    // 3. Scripted protocol errors.
    if let code = s.errorCode {
        switch (s.op, code) {
        case ("SendMessage", -32005), ("SendMessage", -32006),
             ("SendMessage", -32008), ("SendMessage", -32009):
            return nil // thrown from handleMessage; framework maps the code
        case ("GetTask", -32001):
            return nil // framework natural behavior for unknown task id
        case ("CancelTask", -32002):
            return nil // framework natural behavior once a terminal task is seeded
        case ("GetExtendedAgentCard", -32007):
            return nil // scripted extendedAgentCard hook returns nil; framework throws -32007 (requires an authenticated caller — unauthenticated requests get the framework's -32010 gate first)
        case ("CreateTaskPushNotificationConfig", -32003):
            return "push-notification CRUD is framework-owned and unconditionally supported by this SDK (it creates the config or emits -32001 for unknown tasks); -32003 cannot be produced"
        case ("SubscribeToTask", -32004):
            return "SubscribeToTask is framework-owned (task-store lookup + snapshot stream); it emits -32001 for unknown ids and never -32004"
        default:
            return "error \(code) for op \(s.op) cannot be produced through this SDK's handler abstraction or natural framework behavior"
        }
    }

    // 4. Successful responses.
    switch s.op {
    case "SendMessage":
        return s.respond != nil ? nil : "scenario has no scripted response"
    case "SendStreamingMessage", "SubscribeToTask":
        return s.sse != nil ? nil : "scenario has no scripted SSE frames"
    case "GetTask", "CancelTask":
        // Served from the framework's task store after a verified loopback
        // seed through the SDK's own SendMessage path.
        return s.respond != nil ? nil : "scenario has no scripted response"
    case "ListTasks":
        return "ListTasks is framework-owned and served from the SDK's task store; the scripted pagination envelope (nextPageToken \"\(s.respond?["nextPageToken"]?.stringValue ?? "?")\", totalSize \(s.respond?["totalSize"]?.intValue ?? -1)) cannot be produced (the store generates numeric offset tokens and real counts)"
    case "ResolveCard", "SelectInterface":
        return s.card != nil ? nil : "scenario has no card"
    case "RawRequest":
        return nil // SDK envelope layer is exactly what is under test
    default:
        return "op \(s.op) is not supported by this harness"
    }
}
