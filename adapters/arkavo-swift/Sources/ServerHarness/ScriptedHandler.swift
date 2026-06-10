// The scripted `A2ARequestHandler`: every operation records the typed request
// (re-encoded with the SDK encoder, for `GET /observed`) and then answers from
// the armed scenario's `server` section. The SDK's `JSONRPCDispatcher` still
// owns envelope parsing, method routing, params decode, error-code mapping,
// and SSE framing — exactly the layers under test.

import A2A
import A2AServer
import Foundation

struct ScriptedHandler: A2ARequestHandler {
    let state: ScenarioState

    // MARK: - Operations

    func sendMessage(_ request: SendMessageRequest) async throws -> SendMessageResponse {
        await record(request)
        return try await unaryResult()
    }

    func sendStreamingMessage(
        _ request: SendMessageRequest
    ) async throws -> AsyncThrowingStream<StreamResponse, Error> {
        await record(request)
        return try await scriptedStream()
    }

    func getTask(_ request: GetTaskRequest) async throws -> A2ATask {
        await record(request)
        return try await unaryResult()
    }

    func listTasks(_ request: ListTasksRequest) async throws -> ListTasksResponse {
        await record(request)
        return try await unaryResult()
    }

    func cancelTask(_ request: CancelTaskRequest) async throws -> A2ATask {
        await record(request)
        return try await unaryResult()
    }

    func subscribeToTask(
        _ request: SubscribeToTaskRequest
    ) async throws -> AsyncThrowingStream<StreamResponse, Error> {
        await record(request)
        return try await scriptedStream()
    }

    func createTaskPushNotificationConfig(
        _ config: TaskPushNotificationConfig
    ) async throws -> TaskPushNotificationConfig {
        await record(config)
        return try await unaryResult()
    }

    func getTaskPushNotificationConfig(
        _ request: GetTaskPushNotificationConfigRequest
    ) async throws -> TaskPushNotificationConfig {
        await record(request)
        return try await unaryResult()
    }

    func listTaskPushNotificationConfigs(
        _ request: ListTaskPushNotificationConfigsRequest
    ) async throws -> ListTaskPushNotificationConfigsResponse {
        await record(request)
        return try await unaryResult()
    }

    func deleteTaskPushNotificationConfig(
        _ request: DeleteTaskPushNotificationConfigRequest
    ) async throws {
        await record(request)
        let script = try await currentScript()
        if let error = script.error {
            throw scriptedError(error)
        }
        // `respond` for delete is google.protobuf.Empty; nothing to decode.
    }

    func getExtendedAgentCard(_ request: GetExtendedAgentCardRequest) async throws -> AgentCard {
        await record(request)
        return try await unaryResult()
    }

    // MARK: - Script plumbing

    private func record<Request: Encodable>(_ request: Request) async {
        guard let data = try? A2AJSON.encoder().encode(request) else { return }
        await state.record(params: data)
    }

    private func currentScript() async throws -> ScenarioScript {
        guard let script = await state.currentScript() else {
            throw A2AError(.internalError, message: "harness: no scenario selected")
        }
        return script
    }

    private func scriptedError(_ errorJSON: Data) -> A2AError {
        guard
            let object = try? A2AJSON.decoder().decode(JSONRPCErrorObject.self, from: errorJSON)
        else {
            return A2AError(.internalError, message: "harness: unparsable scripted error")
        }
        return A2AError(object)
    }

    private func unaryResult<Result: Decodable>() async throws -> Result {
        let script = try await currentScript()
        if let error = script.error {
            throw scriptedError(error)
        }
        guard let respond = script.respond else {
            throw A2AError(
                .internalError,
                message: "harness: scenario \(script.id) has no respond script for a unary op")
        }
        return try A2AJSON.decoder().decode(Result.self, from: respond)
    }

    /// Streaming script. A scripted `error` is delivered by *failing the
    /// stream* rather than throwing from the handler: the dispatcher then
    /// frames the protocol error as an SSE event, which is the only shape the
    /// SDK client surfaces from a streaming method (a plain JSON error body on
    /// a 200 response would parse as zero SSE events).
    private func scriptedStream() async throws -> AsyncThrowingStream<StreamResponse, Error> {
        let script = try await currentScript()
        if let error = script.error {
            let a2aError = scriptedError(error)
            return AsyncThrowingStream { continuation in
                continuation.finish(throwing: a2aError)
            }
        }
        guard let frames = script.sse else {
            throw A2AError(
                .internalError,
                message: "harness: scenario \(script.id) has no sse script for a streaming op")
        }
        let events = try frames.map {
            try A2AJSON.decoder().decode(StreamResponse.self, from: $0)
        }
        return AsyncThrowingStream { continuation in
            for event in events {
                continuation.yield(event)
            }
            continuation.finish()
        }
    }
}
