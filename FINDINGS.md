# Findings — initial 3×3 matrix run (2026-06-10, macOS)

29 scenarios × 9 cells = 261 results: **195 pass / 42 fail / 24 skip / 0 harness errors.**
All 42 failures decompose into the nine root causes below — every one a real SDK behavior, none a harness artifact. Raw evidence: `reports/results.ndjson` (failures carry wire captures), per-cell detail in `reports/cells/`.

Versions: `a2aproject/a2a-rs @ 7676ec9f` · `arkavo-ai/a2a-swift 0.1.0` · `tolgaki/a2a-swift @ 5b0afd92` + `tolgaki/a2a-swift-server @ 0a1db4f7`.

## a2aproject/a2a-rs

1. **Client never transmits `tenant`.** No injection path exists (`tenant: None` throughout `a2a-client`); spec §8.3.2 rule 4 requires echoing the selected interface's tenant on every request. Fails `discovery/tenant-echo` against every server that can observe it — including its own.
2. **protojson decode rejects unknown fields.** A result carrying forward-compatible extra fields is rejected with a local deserialize error; §5.7 says implementations SHOULD ignore unrecognized fields. Fails `edge/unknown-field-tolerance` as a client.
3. **SSE client silently swallows plain-JSON error responses to streaming calls.** `parse_sse_bytes` discards any unterminated buffer at EOF, so a JSON-RPC error envelope answering `SubscribeToTask` becomes an empty, successfully-closed stream. Fails `errors/unsupported-operation-32004` whenever the server replies with a plain JSON error (including its own server).

## tolgaki/a2a-swift (+ a2a-swift-server)

4. **Server emits v0.3-style `kind`-discriminated flat result envelopes on v1.0 method names** (`{"result":{"kind":"task",...}}` instead of the proto oneof `{"result":{"task":{...}}}`), and its SSE frames carry the same shapes. Strict v1.0 clients reject every result and every stream; this single cause accounts for 14 of the 42 failures. (Previously reported in [a2aproject/A2A#1931](https://github.com/a2aproject/A2A/discussions/1931); his own client accepts both shapes, which is why self-pairs pass.)
5. **Client emits v0.3-flavored request shapes.** a2a-rs's strict decoder rejects them with −32700 (9 failures); the lenient arkavo decoder accepts most, but the push-config request shape is structurally different (missing required `url` at the v1.0 position).
6. **Client interface selection ignores binding compatibility** — takes `supportedInterfaces.first` and maps unknown bindings to REST. §8.3.2 requires selecting the first *supported* transport. Fails `discovery/interface-selection` against every server, including itself.
7. **Server maps undecodable params to −32603** (internal error) instead of −32602 (invalid params).
8. **Server gates `GetExtendedAgentCard` behind authentication with an off-spec −32010 code**; spec defines −32007 for the unconfigured case and no −32010 anywhere.

## arkavo-ai/a2a-swift

9. **Streaming client ignores Content-Type on error responses.** When a server answers a streaming request with a plain-JSON error envelope (as a2a-rs does), the SSE parser sees no `data:` lines and the client yields an empty, successfully-closed stream instead of surfacing the error — the same defect class as finding 3. Fails `errors/unsupported-operation-32004` against a2a-rs. *Fix planned in a2a-swift.*

## Spec gap worth raising upstream

Findings 3 and 9 share a root ambiguity: **§9.4.2 does not say how a pre-stream protocol error is delivered to a streaming request.** a2a-rs answers with a plain JSON envelope (`application/json`); arkavo-swift SSE-frames the error as an event. Each choice breaks somebody's client. The spec should mandate one (or require clients to branch on Content-Type).

## Skips (24)

Honest capability gaps, not failures: the typed Rust/Swift handlers can't inject raw JSON (`edge/unknown-field-tolerance` as server ×2 impls), a2a-rs's `AgentInterface` type can't represent a card without `protocolVersion` (server side), and tolgaki's high-level `A2AHandler` doesn't expose framework-owned ops for scripting or observation (6 scenarios). Per-cell reasons are recorded in the results.
