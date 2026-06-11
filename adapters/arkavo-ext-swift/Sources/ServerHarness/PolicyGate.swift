// arkavo-ext: scenario-keyed policy gate (POLICY-HARNESS.md).
//
// While an `arkavo/policy/*` scenario is selected, dispatch routes through
// ArkavoA2APolicy's GatedRequestHandler over the same scripted handler, with
// the shared reference evaluator — plus, for gate-deny-task-op-32099 only, a
// harness-armed deny-all-task-ops rule (TM-001). The arming lives HERE, in
// the harness; the extension layer's code paths stay scenario-blind
// (identity auth-arming pattern).

import A2A
import A2AServer
import ArkavoA2APolicy
import Foundation

/// The harness-side composite evaluator: while gate-deny-task-op-32099 is
/// selected, deny every non-message-carrying (task-management) op with the
/// TM-001 harness rule; otherwise fall through to the shared
/// ReferenceEvaluator (which never denies message-free operations).
struct HarnessPolicyEvaluator: PolicyEvaluator {
    let state: ScenarioState

    func evaluate(_ context: GateContext) async -> PolicyDecision {
        if await state.policyTaskOpDenyArmed(), !context.op.isMessageCarrying {
            return .deny(
                PolicyRejection(
                    policyId: "conformance:reference@v1",
                    ruleId: "TM-001",
                    reason: "task-management refused by policy",
                    advisory: false
                ))
        }
        return await ReferenceEvaluator().evaluate(context)
    }
}

/// Routing shim in front of the SDK's JSONRPCDispatcher (the dispatcher is
/// built once at startup, so BOTH dispatch paths are built up front and this
/// outer handler delegates per ScenarioState — the handler-layer mirror of
/// the identity auth-arming route check):
///
/// - policy armed (`arkavo/policy/*` selected): record the decoded request
///   for `GET /observed` (a denied request never reaches the inner scripted
///   handler, which normally does the recording), then dispatch through the
///   GatedRequestHandler over the scripted handler;
/// - not armed: the untouched vanilla scripted path (degradation posture).
struct PolicyRoutingHandler: A2ARequestHandler {
    let state: ScenarioState
    let plain: ScriptedHandler
    let gated: GatedRequestHandler<ScriptedHandler, HarnessPolicyEvaluator>

    init(state: ScenarioState, scripted: ScriptedHandler) {
        self.state = state
        self.plain = scripted
        self.gated = GatedRequestHandler(
            wrapping: scripted, evaluator: HarnessPolicyEvaluator(state: state))
    }

    // MARK: - Operations

    func sendMessage(_ request: SendMessageRequest) async throws -> SendMessageResponse {
        if await armed(observing: request) {
            return try await gated.sendMessage(request)
        }
        return try await plain.sendMessage(request)
    }

    func sendStreamingMessage(
        _ request: SendMessageRequest
    ) async throws -> AsyncThrowingStream<StreamResponse, Error> {
        if await armed(observing: request) {
            return try await gated.sendStreamingMessage(request)
        }
        return try await plain.sendStreamingMessage(request)
    }

    func getTask(_ request: GetTaskRequest) async throws -> A2ATask {
        if await armed(observing: request) {
            return try await gated.getTask(request)
        }
        return try await plain.getTask(request)
    }

    func listTasks(_ request: ListTasksRequest) async throws -> ListTasksResponse {
        if await armed(observing: request) {
            return try await gated.listTasks(request)
        }
        return try await plain.listTasks(request)
    }

    func cancelTask(_ request: CancelTaskRequest) async throws -> A2ATask {
        if await armed(observing: request) {
            return try await gated.cancelTask(request)
        }
        return try await plain.cancelTask(request)
    }

    func subscribeToTask(
        _ request: SubscribeToTaskRequest
    ) async throws -> AsyncThrowingStream<StreamResponse, Error> {
        if await armed(observing: request) {
            return try await gated.subscribeToTask(request)
        }
        return try await plain.subscribeToTask(request)
    }

    func createTaskPushNotificationConfig(
        _ config: TaskPushNotificationConfig
    ) async throws -> TaskPushNotificationConfig {
        if await armed(observing: config) {
            return try await gated.createTaskPushNotificationConfig(config)
        }
        return try await plain.createTaskPushNotificationConfig(config)
    }

    func getTaskPushNotificationConfig(
        _ request: GetTaskPushNotificationConfigRequest
    ) async throws -> TaskPushNotificationConfig {
        if await armed(observing: request) {
            return try await gated.getTaskPushNotificationConfig(request)
        }
        return try await plain.getTaskPushNotificationConfig(request)
    }

    func listTaskPushNotificationConfigs(
        _ request: ListTaskPushNotificationConfigsRequest
    ) async throws -> ListTaskPushNotificationConfigsResponse {
        if await armed(observing: request) {
            return try await gated.listTaskPushNotificationConfigs(request)
        }
        return try await plain.listTaskPushNotificationConfigs(request)
    }

    func deleteTaskPushNotificationConfig(
        _ request: DeleteTaskPushNotificationConfigRequest
    ) async throws {
        if await armed(observing: request) {
            return try await gated.deleteTaskPushNotificationConfig(request)
        }
        return try await plain.deleteTaskPushNotificationConfig(request)
    }

    func getExtendedAgentCard(_ request: GetExtendedAgentCardRequest) async throws -> AgentCard {
        if await armed(observing: request) {
            return try await gated.getExtendedAgentCard(request)
        }
        return try await plain.getExtendedAgentCard(request)
    }

    // MARK: - Arming + observation

    /// True when the policy gate is armed; records the request for
    /// `GET /observed` in that case (the inner scripted handler also records
    /// on the allow path — same bytes, idempotent overwrite).
    private func armed<Request: Encodable>(observing request: Request) async -> Bool {
        guard await state.policyArmed() else { return false }
        if let data = try? A2AJSON.encoder().encode(request) {
            await state.record(params: data)
        }
        return true
    }
}
