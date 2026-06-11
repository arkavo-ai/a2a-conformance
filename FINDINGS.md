# Findings — initial 3×3 matrix run (2026-06-10, macOS)

29 scenarios × 9 cells = 261 results: **196 pass / 41 fail / 24 skip / 0 harness errors** (with arkavo a2a-swift 0.1.1; the initial run against 0.1.0 was 195/42 — see finding 9).
All failures decompose into the root causes below — every one a real SDK behavior, none a harness artifact. Raw evidence: `reports/results.ndjson` (failures carry wire captures), per-cell detail in `reports/cells/`.

Versions: `a2aproject/a2a-rs @ 7676ec9f` · `arkavo-ai/a2a-swift 0.1.1` (initial run: 0.1.0) · `tolgaki/a2a-swift @ 5b0afd92` + `tolgaki/a2a-swift-server @ 0a1db4f7`.

## a2aproject/a2a-rs

1. **Client never transmits `tenant`.** No injection path exists (`tenant: None` throughout `a2a-client`); spec §8.3.2 rule 4 requires echoing the selected interface's tenant on every request. Fails `discovery/tenant-echo` against every server that can observe it — including its own.
2. **protojson decode rejects unknown fields.** A result carrying forward-compatible extra fields is rejected with a local deserialize error; §5.7 says implementations SHOULD ignore unrecognized fields. Fails `edge/unknown-field-tolerance` as a client.
3. **SSE client silently swallows plain-JSON error responses to streaming calls.** `parse_sse_bytes` discards any unterminated buffer at EOF, so a JSON-RPC error envelope answering `SubscribeToTask` becomes an empty, successfully-closed stream. Fails `errors/unsupported-operation-32004` whenever the server replies with a plain JSON error (including its own server).
4. **Client cannot surface HTTP-level auth failures.** On a 401 with an empty body, `JsonRpcTransport` unconditionally attempts JSON parsing and fabricates `A2AError { code: -32603, message: "failed to parse JSON-RPC response: ..." }` — the status code and `WWW-Authenticate` challenge never reach the caller, so an SDK consumer cannot distinguish "credentials rejected" from "garbled response". Found while wiring CWT authentication (binding-level 401/403 per A2A §7.4) into the extension matrix.

## tolgaki/a2a-swift (+ a2a-swift-server)

5. **Server emits v0.3-style `kind`-discriminated flat result envelopes on v1.0 method names** (`{"result":{"kind":"task",...}}` instead of the proto oneof `{"result":{"task":{...}}}`), and its SSE frames carry the same shapes. Strict v1.0 clients reject every result and every stream; this single cause accounts for 14 failures in the core matrix. (Previously reported in [a2aproject/A2A#1931](https://github.com/a2aproject/A2A/discussions/1931); his own client accepts both shapes, which is why self-pairs pass.)
6. **Client emits v0.3-flavored request shapes.** a2a-rs's strict decoder rejects them with −32700 (9 failures); the lenient arkavo decoder accepts most, but the push-config request shape is structurally different (missing required `url` at the v1.0 position).
7. **Client interface selection ignores binding compatibility** — takes `supportedInterfaces.first` and maps unknown bindings to REST. §8.3.2 requires selecting the first *supported* transport. Fails `discovery/interface-selection` against every server, including itself.
8. **Server maps undecodable params to −32603** (internal error) instead of −32602 (invalid params).
9. **Server gates `GetExtendedAgentCard` behind authentication with an off-spec −32010 code**; spec defines −32007 for the unconfigured case and no −32010 anywhere.

Additionally (build-level, found by CI rather than the matrix): **the server stack does not compile on Linux** — its `a2a-client-swift` 1.0.22 dependency lacks `FoundationNetworking` imports, so `HTTPURLResponse`/`URLSession` are unresolved on corelibs-foundation. (The newer `tolgaki/a2a-swift` client repo does have Linux CI; the server's pinned dependency predates it.) The Linux leg of this repo's CI therefore runs the rust-a2a × arkavo-swift 2×2 only.

## arkavo-ai/a2a-swift

10. **Streaming client dropped plain-JSON error envelopes** (same defect class as finding 3): a plain-JSON error answering a streaming request read as an empty, successfully-closed stream. Found by this matrix against a2a-rs in 0.1.0; **fixed in [a2a-swift 0.1.1](https://github.com/arkavo-ai/a2a-swift/releases/tag/0.1.1)** — `errors/unsupported-operation-32004` vs a2a-rs is now green, which is also the harness's first end-to-end find→fix→verify loop.

## Spec gap worth raising upstream

Findings 3 and 10 share a root ambiguity: **§9.4.2 does not say how a pre-stream protocol error is delivered to a streaming request.** a2a-rs answers with a plain JSON envelope (`application/json`); arkavo-swift SSE-frames the error as an event. Each choice breaks somebody's client. The spec should mandate one (or require clients to branch on Content-Type).

## Skips (24)

Honest capability gaps, not failures: the typed Rust/Swift handlers can't inject raw JSON (`edge/unknown-field-tolerance` as server ×2 impls), a2a-rs's `AgentInterface` type can't represent a card without `protocolVersion` (server side), and tolgaki's high-level `A2AHandler` doesn't expose framework-owned ops for scripting or observation (6 scenarios). Per-cell reasons are recorded in the results.
