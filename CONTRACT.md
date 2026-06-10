# Adapter ops contract (v1)

Every implementation contributes two executables. The runner is the only orchestrator; harnesses are deliberately dumb. All conformance plumbing (scenario selection, observation) rides over a **control channel**, so SDKs need no special introspection hooks.

## Server harness

```
server-harness --scenarios <dir> --public-base-url <URL>
```

1. Loads every `scenarios/**/*.json` (only the `server` sections and `client.op` are relevant to it).
2. Binds two ephemeral ports: the **A2A port** (served by the SDK under test) and the **control port** (plain HTTP, harness-owned, never the SDK).
3. Prints exactly one line to stdout, then nothing else (logs → stderr):

```json
READY {"port": 49152, "controlPort": 49153, "baseUrl": "http://127.0.0.1:49152"}
```

4. Control API (all JSON):
   - `POST /select` body `{"scenario": "<id>"}` → `{"ok": true}` or `{"ok": false, "reason": "<why this harness cannot serve this scenario>"}`. Selecting a scenario:
     - arms the scripted handler: subsequent A2A requests are answered from that scenario's `server` section (`respond` / `error` / `sse` / `rawResult`),
     - switches the card served at `/.well-known/agent-card.json` to `server.card` if present (else a default card), with every `{{baseUrl}}` placeholder replaced by `--public-base-url`,
     - resets the per-scenario observation buffer.
   - `GET /observed` → `{"params": <JSON|null>}` — the params of the last A2A request as **the server SDK decoded and re-encoded them** (wire JSON). `null` if the SDK layer does not expose the request to the harness. Used for `expectRequest` checks (e.g. tenant echo); raw bytes for failure diagnosis come from the runner's capture proxy instead.
5. The A2A endpoint is the SDK's own server layer (router/dispatcher). The scripted logic plugs in at the SDK's handler abstraction — the SDK still owns envelope parsing, method routing, type decode/encode, error-code mapping, and SSE framing. `RawRequest` scenarios have no `server` section *by design*: they probe exactly that SDK layer (unknown method → −32601, undecodable params → −32602).
6. Exits on stdin EOF.

A harness that cannot serve a scenario (e.g. the SDK's handler abstraction doesn't allow scripting that operation) answers `/select` with `ok:false`; the runner records `skip` for that cell, never `fail`.

## Client harness

```
client-harness
```

Reads NDJSON lines on stdin; for each, performs the op **through the SDK's native API** and emits one NDJSON outcome line on stdout (schema: `schema/harness-outcome.schema.json`). Logs → stderr. Exits on stdin EOF.

Input line:

```json
{"scenario": "core/get-task", "baseUrl": "http://127.0.0.1:49160", "op": "GetTask",
 "params": {"id": "t-get-1"}, "rawBody": null, "timeoutMs": 30000}
```

Op semantics:

| op | what the harness does | outcome.kind |
|---|---|---|
| `SendMessage`, `GetTask`, `ListTasks`, `CancelTask`, `CreateTaskPushNotificationConfig`, `GetExtendedAgentCard` | decode `params` into SDK request types (or pass wire JSON if the SDK takes it), call the method | `result` with `value` = the SDK's result re-encoded to wire JSON; or `error` with `errorCode`/`errorMessage` if the SDK surfaced a protocol error |
| `SendStreamingMessage`, `SubscribeToTask` | call the streaming method, collect all events until stream end | `stream` with ordered `events: [{"kind": "task"|"message"|"statusUpdate"|"artifactUpdate", "value": <wire JSON>}]`; `error` if the call itself failed |
| `ResolveCard` | fetch + decode `{baseUrl}/.well-known/agent-card.json` via the SDK's resolver | `card` with `value` = decoded card re-encoded |
| `SelectInterface` | resolve the card, then run the SDK's interface-selection logic | `interface` with `value` = the selected `AgentInterface` re-encoded (or `error` if none) |
| `RawRequest` | POST `rawBody` verbatim to `baseUrl` with `Content-Type: application/json` (plain HTTP — the only op that bypasses the SDK) | `error` with the JSON-RPC error code from the response envelope; `result` if the server unexpectedly succeeded |

Rules:

- **`value` is the SDK's decoded view re-encoded with the SDK's own encoder.** This is the point: lenient/strict decode behavior is the thing under test. Do not echo the wire bytes back.
- A protocol error surfaced by the SDK is `outcome.kind = "error"` (with `errorCode` null if the SDK threw a non-protocol error such as a decode failure — put the description in `errorMessage`).
- A harness crash/timeout/bug is `outcome.kind = "harness-error"`.
- An op the SDK genuinely cannot perform is `outcome.kind = "unsupported"` → runner records `skip`.
- `impl` carries `{"name": "<matrix.toml name>", "version": "<SDK version>"}`.

## Runner sequencing (per matrix cell)

1. Bind capture-proxy listener (port P).
2. Spawn server harness with `--public-base-url http://127.0.0.1:P`; read `READY` (port S, control C).
3. Point proxy at S. The proxy is a transparent TCP tap: all client traffic flows through it; captured bytes are tagged with the currently selected scenario and attached to failing results. Checks never depend on the proxy.
4. Spawn client harness. For each scenario applicable to the cell (`appliesTo` + tag filters):
   `POST control/select` → if `ok:false` record `skip` → else write input line (with `baseUrl = http://127.0.0.1:P`) → await outcome line (timeout → `error`) → if scenario has `expectRequest`, `GET control/observed` → evaluate expectations → record result.
5. Close client stdin; EOF server stdin; await exits.

Scenario execution is strictly sequential per cell — harnesses never see concurrent scenarios.
