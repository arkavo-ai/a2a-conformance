#!/usr/bin/env python3
"""One-time authoring helper: writes the a2a-conformance scenario corpus."""
import json, os, base64

ROOT = "/Users/arkavo/Projects/intelligence/a2a-conformance/scenarios"

def msg(text, mid="m-1"):
    return {"messageId": mid, "role": "ROLE_USER", "parts": [{"text": text}]}

def W(sid, obj):
    obj["id"] = sid
    path = os.path.join(ROOT, sid + ".json")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    # stable key order for reviewability
    ordered = {k: obj[k] for k in
               ["id", "title", "spec", "client", "server", "expect", "expectRequest", "tags", "appliesTo"]
               if k in obj}
    with open(path, "w") as f:
        json.dump(ordered, f, indent=2, ensure_ascii=False)
        f.write("\n")

# ---------------------------------------------------------------- core (6)
W("core/send-message-returns-task", {
    "title": "SendMessage where the agent opens a task",
    "spec": "§3.1.1, §9.4.1",
    "client": {"op": "SendMessage", "params": {"message": msg("run the report")}},
    "server": {"respond": {"task": {
        "id": "t-smrt-1", "contextId": "c-1",
        "status": {"state": "TASK_STATE_COMPLETED", "timestamp": "2026-01-01T00:00:00.000Z"},
        "artifacts": [{"artifactId": "a-1", "name": "report",
                       "parts": [{"text": "report finished", "mediaType": "text/plain"}]}]}}},
    "expect": {"kind": "result", "checks": [
        {"path": "$.task.id", "equals": "t-smrt-1"},
        {"path": "$.task.status.state", "equals": "TASK_STATE_COMPLETED"},
        {"path": "$.task.artifacts[0].parts[0].text", "equals": "report finished"}]},
    "tags": ["core", "v1.0"]})

W("core/send-message-returns-message", {
    "title": "SendMessage where the agent replies with a direct message",
    "spec": "§3.1.1, §9.4.1",
    "client": {"op": "SendMessage", "params": {"message": msg("hello")}},
    "server": {"respond": {"message": {
        "messageId": "m-reply-1", "role": "ROLE_AGENT",
        "parts": [{"text": "hello back", "mediaType": "text/plain"}]}}},
    "expect": {"kind": "result", "checks": [
        {"path": "$.message.role", "equals": "ROLE_AGENT"},
        {"path": "$.message.parts[0].text", "equals": "hello back"}]},
    "tags": ["core", "v1.0"]})

W("core/get-task", {
    "title": "GetTask returns the current task state",
    "spec": "§3.1.3, §9.4.3",
    "client": {"op": "GetTask", "params": {"id": "t-get-1"}},
    "server": {"respond": {
        "id": "t-get-1", "contextId": "c-1",
        "status": {"state": "TASK_STATE_WORKING", "timestamp": "2026-01-01T00:00:01.000Z"}}},
    "expect": {"kind": "result", "checks": [
        {"path": "$.id", "equals": "t-get-1"},
        {"path": "$.status.state", "equals": "TASK_STATE_WORKING"}]},
    "tags": ["core", "v1.0"]})

W("core/get-task-history-length", {
    "title": "GetTask transmits historyLength and decodes returned history",
    "spec": "§3.1.3 (history_length)",
    "client": {"op": "GetTask", "params": {"id": "t-hist-1", "historyLength": 2}},
    "server": {"respond": {
        "id": "t-hist-1", "contextId": "c-1",
        "status": {"state": "TASK_STATE_COMPLETED"},
        "history": [msg("first", "m-h1"), msg("second", "m-h2")]}},
    "expect": {"kind": "result", "checks": [
        {"path": "$.history", "count": 2},
        {"path": "$.history[1].messageId", "equals": "m-h2"}]},
    "expectRequest": {"checks": [{"path": "$.params.historyLength", "equals": 2}]},
    "tags": ["core", "v1.0"]})

W("core/list-tasks-pagination", {
    "title": "ListTasks returns tasks plus pagination fields",
    "spec": "§3.1.4, §9.4.4",
    "client": {"op": "ListTasks", "params": {"pageSize": 2}},
    "server": {"respond": {
        "tasks": [
            {"id": "t-l1", "contextId": "c-1", "status": {"state": "TASK_STATE_COMPLETED"}},
            {"id": "t-l2", "contextId": "c-1", "status": {"state": "TASK_STATE_WORKING"}}],
        "nextPageToken": "page-2", "pageSize": 2, "totalSize": 5}},
    "expect": {"kind": "result", "checks": [
        {"path": "$.tasks", "count": 2},
        {"path": "$.tasks[1].id", "equals": "t-l2"},
        {"path": "$.nextPageToken", "equals": "page-2"}]},
    "tags": ["core", "v1.0"]})

W("core/cancel-task", {
    "title": "CancelTask returns the task in canceled state",
    "spec": "§3.1.5, §9.4.5",
    "client": {"op": "CancelTask", "params": {"id": "t-c1"}},
    "server": {"respond": {
        "id": "t-c1", "contextId": "c-1",
        "status": {"state": "TASK_STATE_CANCELED", "timestamp": "2026-01-01T00:00:02.000Z"}}},
    "expect": {"kind": "result", "checks": [
        {"path": "$.id", "equals": "t-c1"},
        {"path": "$.status.state", "equals": "TASK_STATE_CANCELED"}]},
    "tags": ["core", "v1.0"]})

# ------------------------------------------------------------ streaming (4)
W("streaming/sse-status-updates", {
    "title": "Streaming send: task snapshot then status updates to terminal",
    "spec": "§3.1.2, §9.4.2",
    "client": {"op": "SendStreamingMessage", "params": {"message": msg("stream status")}},
    "server": {"sse": [
        {"task": {"id": "t-st-1", "contextId": "c-1", "status": {"state": "TASK_STATE_SUBMITTED"}}},
        {"statusUpdate": {"taskId": "t-st-1", "contextId": "c-1",
                          "status": {"state": "TASK_STATE_WORKING"}}},
        {"statusUpdate": {"taskId": "t-st-1", "contextId": "c-1",
                          "status": {"state": "TASK_STATE_COMPLETED"}}}]},
    "expect": {"kind": "stream",
               "streamOrder": ["task", "statusUpdate", "statusUpdate"],
               "checks": [{"path": "$.events[2].statusUpdate.status.state",
                           "equals": "TASK_STATE_COMPLETED"}]},
    "tags": ["streaming", "v1.0"]})

W("streaming/sse-artifact-chunks-append", {
    "title": "Streaming send: artifact delivered in chunks with append/lastChunk",
    "spec": "§3.1.2, TaskArtifactUpdateEvent.append/last_chunk",
    "client": {"op": "SendStreamingMessage", "params": {"message": msg("stream artifact")}},
    "server": {"sse": [
        {"task": {"id": "t-sa-1", "contextId": "c-1", "status": {"state": "TASK_STATE_WORKING"}}},
        {"artifactUpdate": {"taskId": "t-sa-1", "contextId": "c-1",
                            "artifact": {"artifactId": "a-sa-1",
                                         "parts": [{"text": "chunk one ", "mediaType": "text/plain"}]}}},
        {"artifactUpdate": {"taskId": "t-sa-1", "contextId": "c-1", "append": True, "lastChunk": True,
                            "artifact": {"artifactId": "a-sa-1",
                                         "parts": [{"text": "chunk two", "mediaType": "text/plain"}]}}},
        {"statusUpdate": {"taskId": "t-sa-1", "contextId": "c-1",
                          "status": {"state": "TASK_STATE_COMPLETED"}}}]},
    "expect": {"kind": "stream",
               "streamOrder": ["task", "artifactUpdate", "artifactUpdate", "statusUpdate"],
               "checks": [
                   {"path": "$.events[2].artifactUpdate.append", "equals": True},
                   {"path": "$.events[2].artifactUpdate.lastChunk", "equals": True},
                   {"path": "$.events[2].artifactUpdate.artifact.artifactId", "equals": "a-sa-1"}]},
    "tags": ["streaming", "v1.0"]})

W("streaming/sse-message-only-stream", {
    "title": "Streaming send answered by a single message event",
    "spec": "§3.1.2 (StreamResponse.message)",
    "client": {"op": "SendStreamingMessage", "params": {"message": msg("quick question")}},
    "server": {"sse": [
        {"message": {"messageId": "m-s1", "role": "ROLE_AGENT",
                     "parts": [{"text": "quick answer", "mediaType": "text/plain"}]}}]},
    "expect": {"kind": "stream",
               "streamOrder": ["message"],
               "checks": [{"path": "$.events[0].message.parts[0].text", "equals": "quick answer"}]},
    "tags": ["streaming", "v1.0"]})

W("streaming/sse-terminal-close", {
    "title": "Stream closes after a terminal status update; client must not hang",
    "spec": "§3.1.2 (terminal states end the stream)",
    "client": {"op": "SendStreamingMessage", "params": {"message": msg("short stream")}},
    "server": {"sse": [
        {"task": {"id": "t-tc-1", "contextId": "c-1", "status": {"state": "TASK_STATE_WORKING"}}},
        {"statusUpdate": {"taskId": "t-tc-1", "contextId": "c-1",
                          "status": {"state": "TASK_STATE_FAILED"}}}]},
    "expect": {"kind": "stream",
               "streamOrder": ["task", "statusUpdate"],
               "checks": [{"path": "$.events[1].statusUpdate.status.state",
                           "equals": "TASK_STATE_FAILED"}]},
    "tags": ["streaming", "v1.0"]})

# --------------------------------------------------------------- errors (11)
def err_scenario(name, code, const, op, params, spec, server_op_note, applies=None):
    s = {
        "title": f"{op} surfaces {const} ({code}) to the caller",
        "spec": spec,
        "client": {"op": op, "params": params},
        "server": {"error": {"code": code, "message": server_op_note}},
        "expect": {"kind": "error", "errorCode": code},
        "tags": ["errors", "v1.0"],
    }
    if applies:
        s["appliesTo"] = applies
    W(f"errors/{name}", s)

err_scenario("task-not-found-32001", -32001, "TaskNotFoundError",
             "GetTask", {"id": "no-such-task"}, "§5.4, §9.5", "Task not found")
err_scenario("task-not-cancelable-32002", -32002, "TaskNotCancelableError",
             "CancelTask", {"id": "t-done"}, "§5.4, §9.5", "Task cannot be canceled")
err_scenario("push-not-supported-32003", -32003, "PushNotificationNotSupportedError",
             "CreateTaskPushNotificationConfig",
             {"taskId": "t-p1", "url": "https://client.example.com/hook"},
             "§5.4, §9.5", "Push Notification is not supported")
err_scenario("unsupported-operation-32004", -32004, "UnsupportedOperationError",
             "SubscribeToTask", {"id": "t-terminal"}, "§5.4, §3.1.6", "This operation is not supported")
err_scenario("content-type-not-supported-32005", -32005, "ContentTypeNotSupportedError",
             "SendMessage", {"message": msg("video request")}, "§5.4, §9.5", "Incompatible content types")
err_scenario("invalid-agent-response-32006", -32006, "InvalidAgentResponseError",
             "SendMessage", {"message": msg("anything")}, "§5.4, §9.5", "Invalid agent response")
err_scenario("extended-card-not-configured-32007", -32007, "ExtendedAgentCardNotConfiguredError",
             "GetExtendedAgentCard", {}, "§5.4, §9.4.8", "Extended agent card not configured")
err_scenario("extension-support-required-32008", -32008, "ExtensionSupportRequiredError",
             "SendMessage", {"message": msg("needs extension")}, "§5.4, §4.6", "Extension support required")
err_scenario("version-not-supported-32009", -32009, "VersionNotSupportedError",
             "SendMessage", {"message": msg("hello")}, "§5.4, §6.4", "Protocol version not supported")

W("errors/method-not-found-32601", {
    "title": "Unknown JSON-RPC method yields -32601 from the server SDK's envelope layer",
    "spec": "§9.5",
    "client": {"op": "RawRequest",
               "rawBody": "{\"jsonrpc\":\"2.0\",\"id\":601,\"method\":\"NoSuchMethod\",\"params\":{}}"},
    "expect": {"kind": "error", "errorCode": -32601},
    "tags": ["errors", "v1.0", "raw"]})

W("errors/invalid-params-32602", {
    "title": "GetTask with missing required id yields -32602 from the server SDK",
    "spec": "§9.5",
    "client": {"op": "RawRequest",
               "rawBody": "{\"jsonrpc\":\"2.0\",\"id\":602,\"method\":\"GetTask\",\"params\":{}}"},
    "expect": {"kind": "error", "errorCode": -32602},
    "tags": ["errors", "v1.0", "raw"]})

# ------------------------------------------------------------ discovery (3)
DEFAULT_SKILL = {"id": "conformance", "name": "Conformance", "description": "Scripted conformance agent.",
                 "tags": ["conformance"]}

W("discovery/well-known-agent-card", {
    "title": "Agent card is served at the well-known URI and decodes",
    "spec": "§8.2, RFC 8615",
    "client": {"op": "ResolveCard"},
    "server": {"card": {
        "name": "Conformance Agent", "description": "Scripted agent for interop testing.",
        "version": "1.2.3",
        "supportedInterfaces": [
            {"url": "{{baseUrl}}", "protocolBinding": "JSONRPC", "protocolVersion": "1.0"}],
        "capabilities": {"streaming": True},
        "defaultInputModes": ["text/plain"], "defaultOutputModes": ["text/plain"],
        "skills": [DEFAULT_SKILL]}},
    "expect": {"kind": "card", "checks": [
        {"path": "$.name", "equals": "Conformance Agent"},
        {"path": "$.version", "equals": "1.2.3"},
        {"path": "$.supportedInterfaces[0].protocolBinding", "equals": "JSONRPC"}]},
    "tags": ["discovery", "v1.0"]})

W("discovery/interface-selection", {
    "title": "Client selects the first compatible JSONRPC interface from a mixed card",
    "spec": "§8.3.2",
    "client": {"op": "SelectInterface"},
    "server": {"card": {
        "name": "Multi Binding Agent", "description": "Declares gRPC first, JSONRPC second.",
        "version": "1.0.0",
        "supportedInterfaces": [
            {"url": "https://grpc.invalid/a2a", "protocolBinding": "GRPC", "protocolVersion": "1.0"},
            {"url": "{{baseUrl}}", "protocolBinding": "JSONRPC", "protocolVersion": "1.0"},
            {"url": "{{baseUrl}}/json", "protocolBinding": "HTTP+JSON", "protocolVersion": "1.0"}],
        "capabilities": {"streaming": True},
        "defaultInputModes": ["text/plain"], "defaultOutputModes": ["text/plain"],
        "skills": [DEFAULT_SKILL]}},
    "expect": {"kind": "interface", "checks": [
        {"path": "$.protocolBinding", "equals": "JSONRPC"}]},
    "tags": ["discovery", "v1.0"]})

W("discovery/tenant-echo", {
    "title": "Client echoes the selected interface's tenant on requests",
    "spec": "§8.3.2 rule 4, SendMessageRequest.tenant",
    "client": {"op": "SendMessage", "params": {"message": msg("tenant check")}},
    "server": {
        "card": {
            "name": "Tenant Agent", "description": "Declares a tenant on its JSONRPC interface.",
            "version": "1.0.0",
            "supportedInterfaces": [
                {"url": "{{baseUrl}}", "protocolBinding": "JSONRPC",
                 "tenant": "tenant-42", "protocolVersion": "1.0"}],
            "capabilities": {"streaming": True},
            "defaultInputModes": ["text/plain"], "defaultOutputModes": ["text/plain"],
            "skills": [DEFAULT_SKILL]},
        "respond": {"task": {"id": "t-ten-1", "contextId": "c-1",
                             "status": {"state": "TASK_STATE_COMPLETED"}}}},
    "expect": {"kind": "result", "checks": [
        {"path": "$.task.id", "equals": "t-ten-1"}]},
    "expectRequest": {"checks": [{"path": "$.params.tenant", "equals": "tenant-42"}]},
    "tags": ["discovery", "v1.0", "tenant"]})

# ----------------------------------------------------------------- edge (5)
W("edge/omitted-default-required-fields", {
    "title": "Card whose interface omits protocolVersion (proto3 default omission) still decodes and selects",
    "spec": "§5.7 vs. proto3 JSON serializer behavior (found in a2a-swift <-> a2a-python interop)",
    "client": {"op": "SelectInterface"},
    "server": {"card": {
        "name": "Proto3 Canonical Agent",
        "description": "Serialized by a proto3-canonical encoder: default-valued fields omitted.",
        "version": "1.0.0",
        "supportedInterfaces": [{"url": "{{baseUrl}}", "protocolBinding": "JSONRPC"}],
        "capabilities": {"streaming": True},
        "defaultInputModes": ["text/plain"], "defaultOutputModes": ["text/plain"],
        "skills": [DEFAULT_SKILL]}},
    "expect": {"kind": "interface", "checks": [
        {"path": "$.protocolBinding", "equals": "JSONRPC"}]},
    "tags": ["edge", "v1.0", "lenient-decode"]})

W("edge/unknown-field-tolerance", {
    "title": "Result carrying unknown fields decodes (forward compatibility, §5.7)",
    "spec": "§5.7 (implementations SHOULD ignore unrecognized fields)",
    "client": {"op": "GetTask", "params": {"id": "t-uf-1"}},
    "server": {"rawResult": json.dumps({
        "id": "t-uf-1", "contextId": "c-1",
        "status": {"state": "TASK_STATE_COMPLETED", "futureStatusField": True},
        "futureTopLevelField": {"nested": [1, 2, 3]}})},
    "expect": {"kind": "result", "checks": [
        {"path": "$.id", "equals": "t-uf-1"},
        {"path": "$.status.state", "equals": "TASK_STATE_COMPLETED"}]},
    "tags": ["edge", "v1.0", "lenient-decode"]})

W("edge/base64-raw-part", {
    "title": "Artifact with raw bytes part round-trips base64 (Part.raw)",
    "spec": "Part.raw proto3 JSON mapping (bytes as base64)",
    "client": {"op": "GetTask", "params": {"id": "t-raw-1"}},
    "server": {"respond": {
        "id": "t-raw-1", "contextId": "c-1",
        "status": {"state": "TASK_STATE_COMPLETED"},
        "artifacts": [{"artifactId": "a-raw-1", "parts": [
            {"raw": base64.b64encode("hello raw bytes".encode()).decode(),
             "filename": "blob.bin", "mediaType": "application/octet-stream"}]}]}},
    "expect": {"kind": "result", "checks": [
        {"path": "$.artifacts[0].parts[0].raw", "equals": base64.b64encode("hello raw bytes".encode()).decode()},
        {"path": "$.artifacts[0].parts[0].filename", "equals": "blob.bin"}]},
    "tags": ["edge", "v1.0"]})

W("edge/unicode-parts", {
    "title": "Text parts with multi-byte, combining, and RTL content survive transport",
    "spec": "JSON string handling; SSE/UTF-8 byte boundaries",
    "client": {"op": "SendMessage", "params": {"message": msg("unicode test")}},
    "server": {"respond": {"message": {
        "messageId": "m-uni-1", "role": "ROLE_AGENT",
        "parts": [{"text": "café 統合テスト 🚀 مرحبا é", "mediaType": "text/plain"}]}}},
    "expect": {"kind": "result", "checks": [
        {"path": "$.message.parts[0].text", "equals": "café 統合テスト 🚀 مرحبا é"}]},
    "tags": ["edge", "v1.0"]})

W("edge/large-history", {
    "title": "Task with 50 history messages decodes completely",
    "spec": "no size limits below transport level",
    "client": {"op": "GetTask", "params": {"id": "t-lh-1"}},
    "server": {"respond": {
        "id": "t-lh-1", "contextId": "c-1",
        "status": {"state": "TASK_STATE_COMPLETED"},
        "history": [msg(f"history message {i:02d}", f"m-lh-{i:02d}") for i in range(50)]}},
    "expect": {"kind": "result", "checks": [
        {"path": "$.history", "count": 50},
        {"path": "$.history[49].messageId", "equals": "m-lh-49"}]},
    "tags": ["edge", "v1.0"]})

count = sum(len(files) for _, _, files in os.walk(ROOT))
print(f"wrote {count} scenario files")
