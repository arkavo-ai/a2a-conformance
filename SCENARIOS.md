# Scenario catalog

Generated view of `scenarios/` — the JSON files are the source of truth.
Regenerate this file with `python3 tools/render_scenarios_md.py` after editing the corpus.

## core (6)

| ID | Title | Spec | Op | Expects |
|---|---|---|---|---|
| `core/cancel-task` | CancelTask returns the task in canceled state | §3.1.5, §9.4.5 | `CancelTask` | result |
| `core/get-task-history-length` | GetTask transmits historyLength and decodes returned history | §3.1.3 (history_length) | `GetTask` | result + request check |
| `core/get-task` | GetTask returns the current task state | §3.1.3, §9.4.3 | `GetTask` | result |
| `core/list-tasks-pagination` | ListTasks returns tasks plus pagination fields | §3.1.4, §9.4.4 | `ListTasks` | result |
| `core/send-message-returns-message` | SendMessage where the agent replies with a direct message | §3.1.1, §9.4.1 | `SendMessage` | result |
| `core/send-message-returns-task` | SendMessage where the agent opens a task | §3.1.1, §9.4.1 | `SendMessage` | result |

## streaming (4)

| ID | Title | Spec | Op | Expects |
|---|---|---|---|---|
| `streaming/sse-artifact-chunks-append` | Streaming send: artifact delivered in chunks with append/lastChunk | §3.1.2, TaskArtifactUpdateEvent.append/last_chunk | `SendStreamingMessage` | stream: task → artifactUpdate → artifactUpdate → statusUpdate |
| `streaming/sse-message-only-stream` | Streaming send answered by a single message event | §3.1.2 (StreamResponse.message) | `SendStreamingMessage` | stream: message |
| `streaming/sse-status-updates` | Streaming send: task snapshot then status updates to terminal | §3.1.2, §9.4.2 | `SendStreamingMessage` | stream: task → statusUpdate → statusUpdate |
| `streaming/sse-terminal-close` | Stream closes after a terminal status update; client must not hang | §3.1.2 (terminal states end the stream) | `SendStreamingMessage` | stream: task → statusUpdate |

## errors (11)

| ID | Title | Spec | Op | Expects |
|---|---|---|---|---|
| `errors/content-type-not-supported-32005` | SendMessage surfaces ContentTypeNotSupportedError (-32005) to the caller | §5.4, §9.5 | `SendMessage` | error `-32005` |
| `errors/extended-card-not-configured-32007` | GetExtendedAgentCard surfaces ExtendedAgentCardNotConfiguredError (-32007) to the caller | §5.4, §9.4.8 | `GetExtendedAgentCard` | error `-32007` |
| `errors/extension-support-required-32008` | SendMessage surfaces ExtensionSupportRequiredError (-32008) to the caller | §5.4, §4.6 | `SendMessage` | error `-32008` |
| `errors/invalid-agent-response-32006` | SendMessage surfaces InvalidAgentResponseError (-32006) to the caller | §5.4, §9.5 | `SendMessage` | error `-32006` |
| `errors/invalid-params-32602` | GetTask with missing required id yields -32602 from the server SDK | §9.5 | `RawRequest` | error `-32602` |
| `errors/method-not-found-32601` | Unknown JSON-RPC method yields -32601 from the server SDK's envelope layer | §9.5 | `RawRequest` | error `-32601` |
| `errors/push-not-supported-32003` | CreateTaskPushNotificationConfig surfaces PushNotificationNotSupportedError (-32003) to the caller | §5.4, §9.5 | `CreateTaskPushNotificationConfig` | error `-32003` |
| `errors/task-not-cancelable-32002` | CancelTask surfaces TaskNotCancelableError (-32002) to the caller | §5.4, §9.5 | `CancelTask` | error `-32002` |
| `errors/task-not-found-32001` | GetTask surfaces TaskNotFoundError (-32001) to the caller | §5.4, §9.5 | `GetTask` | error `-32001` |
| `errors/unsupported-operation-32004` | SubscribeToTask surfaces UnsupportedOperationError (-32004) to the caller | §5.4, §3.1.6 | `SubscribeToTask` | error `-32004` |
| `errors/version-not-supported-32009` | SendMessage surfaces VersionNotSupportedError (-32009) to the caller | §5.4, §6.4 | `SendMessage` | error `-32009` |

## discovery (3)

| ID | Title | Spec | Op | Expects |
|---|---|---|---|---|
| `discovery/interface-selection` | Client selects the first compatible JSONRPC interface from a mixed card | §8.3.2 | `SelectInterface` | interface |
| `discovery/tenant-echo` | Client echoes the selected interface's tenant on requests | §8.3.2 rule 4, SendMessageRequest.tenant | `SendMessage` | result + request check |
| `discovery/well-known-agent-card` | Agent card is served at the well-known URI and decodes | §8.2, RFC 8615 | `ResolveCard` | card |

## edge (5)

| ID | Title | Spec | Op | Expects |
|---|---|---|---|---|
| `edge/base64-raw-part` | Artifact with raw bytes part round-trips base64 (Part.raw) | Part.raw proto3 JSON mapping (bytes as base64) | `GetTask` | result |
| `edge/large-history` | Task with 50 history messages decodes completely | no size limits below transport level | `GetTask` | result |
| `edge/omitted-default-required-fields` | Card whose interface omits protocolVersion (proto3 default omission) still decodes and selects | §5.7 vs. proto3 JSON serializer behavior (found in a2a-swift <-> a2a-python interop) | `SelectInterface` | interface |
| `edge/unicode-parts` | Text parts with multi-byte, combining, and RTL content survive transport | JSON string handling; SSE/UTF-8 byte boundaries | `SendMessage` | result |
| `edge/unknown-field-tolerance` | Result carrying unknown fields decodes (forward compatibility, §5.7) | §5.7 (implementations SHOULD ignore unrecognized fields) | `GetTask` | result |
