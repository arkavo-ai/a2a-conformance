# a2a-conformance

Pairwise **interop matrix** for [Agent2Agent (A2A) protocol](https://a2a-protocol.org) SDK implementations: N client harnesses × M server harnesses, every cell a scenario suite, output a markdown matrix with per-failure wire captures.

## Why this exists

[`a2aproject/a2a-tck`](https://github.com/a2aproject/a2a-tck) validates *one server against the spec*. This project validates *implementation pairs against each other* — which catches a different failure class: **spec-vs-practice divergence**. Two real examples that motivated it, both found wiring SDKs together (neither is visible to self-referential tests):

- proto3-canonical serializers omit default-valued fields even when the spec marks them REQUIRED (`AgentInterface.protocolVersion`), which strict decoders reject — found in a2a-swift ↔ a2a-python interop;
- a server emitting v0.3-style `kind`-discriminated response envelopes on v1.0 method names, undetectable by its own client because that client tolerates both shapes — found in a2a-swift ↔ community-Swift-SDK interop (see [a2aproject/A2A#1931](https://github.com/a2aproject/A2A/discussions/1931)).

Self-cells run as sanity checks but are **not evidence** — self-interop proves nothing; that's the whole point.

## Current matrix

Spec target: **A2A v1.0.1, JSON-RPC binding.**

Latest snapshot (2026-06-10, macOS): **196 pass / 41 fail / 24 skip / 0 harness errors** over 29 scenarios × 9 cells — deterministic across reruns. All failures reduce to nine root causes, each a real SDK divergence with wire evidence: see **[FINDINGS.md](FINDINGS.md)** and [reports/matrix.md](reports/matrix.md). One root cause (a streaming-error bug in arkavo a2a-swift, found by this matrix) is already fixed and verified green in 0.1.1 — the find→fix→verify loop works end to end.

| implementation | client | server |
|---|---|---|
| `rust-a2a` — [a2aproject/a2a-rs](https://github.com/a2aproject/a2a-rs) | `a2a-client` | `a2a-server` (axum) |
| `arkavo-swift` — [arkavo-ai/a2a-swift](https://github.com/arkavo-ai/a2a-swift) | `A2AClient` | `JSONRPCDispatcher` + Hummingbird shim (shim lives in the adapter; the SDK is framework-free) |
| `tolgaki-swift` — [tolgaki/a2a-swift](https://github.com/tolgaki/a2a-swift) + [a2a-swift-server](https://github.com/tolgaki/a2a-swift-server) | `A2AClient` | `A2AServer` (Hummingbird) |

Phase 2 adds `a2a-python` (a2a-sdk ≥ 1.1.0) as the reference oracle: when two implementations disagree, Python breaks the tie.

## Run it

```sh
cargo build --release --manifest-path runner/Cargo.toml
./runner/target/release/a2a-conformance-runner validate          # schema-check the corpus
./runner/target/release/a2a-conformance-runner run               # full matrix
./runner/target/release/a2a-conformance-runner run \
    --cell client=arkavo-swift,server=rust-a2a --tag streaming   # one cell, one tag group
```

Reports land in `reports/`: `matrix.md` (one table per scenario group, cells link to per-cell detail with wire captures for failures) and `results.ndjson` (machine-readable). `--baseline reports/baseline.ndjson` exits non-zero on regressions.

Requirements: Rust ≥ 1.85, Swift ≥ 6.1 (both Swift SDKs build on macOS and Linux).

## How it works

- **Declarative corpus** — `scenarios/**/*.json` (29 scenarios: core, streaming, errors — every §5.4 code, discovery, edge). See [SCENARIOS.md](SCENARIOS.md). Scenario files are validated against `schema/scenario.schema.json` in CI.
- **Scripted servers, not echo agents.** Each server harness plugs a scripted handler into its SDK's handler abstraction; the SDK still owns envelope parsing, routing, type decode/encode, error mapping, and SSE framing. Determinism, plus it's the only way to test client-side handling of deliberately spec-edge input (proto3 default omission, unknown fields).
- **Two binaries per implementation** with a batched NDJSON stdio contract — see [CONTRACT.md](CONTRACT.md). Scenario selection rides a harness-owned control channel, so SDKs need no introspection hooks.
- **Wire capture** — the runner interposes a transparent TCP tap between client and server; failing results carry raw request/response bytes. Checks never depend on the capture (it's the artifact that makes a failure arguable, not the judge).
- **Honest cells** — a harness that can't serve a scenario reports it at selection time → `skip`, never `fail`. v0.3-compat scenarios are tagged and apply only to implementations that claim that compatibility.

### Deviations from naive designs, deliberately

- Checks are evaluated by the runner against each SDK's *decoded-then-re-encoded* view, not raw wire echoes — lenient/strict decode behavior is precisely the thing under test.
- `expectRequest` checks (e.g. tenant echo) read the *server SDK's* decoded view of the request via the control channel; if a server SDK can't expose it, the scenario skips rather than guessing.

## Add your SDK in two binaries

1. Write `server-harness` and `client-harness` per [CONTRACT.md](CONTRACT.md) (any language; ~300–500 lines each in practice).
2. Add an `[implementations.<name>]` entry to `matrix.toml` (build command + two executable paths) and add the name to `[matrix]`.
3. `runner run --cell client=<name>,server=<name>` until your self-pair is green-or-honestly-skipped, then run the full matrix.

PRs adding implementations are welcome — that includes maintainers of the SDKs already in the matrix who want to own their adapters (invited via [#1931](https://github.com/a2aproject/A2A/discussions/1931)).

## Status / roadmap

- [x] v0.1: 3×3 Swift/Rust matrix, 29 scenarios, wire capture, baseline gating (macOS + Linux CI)
- [ ] Phase 2: `a2a-python` oracle row/column; contribute scenario corpus upstream to a2a-tck if wanted
- [ ] Phase 3: push-notification webhook delivery (needs a receiver harness); gRPC and HTTP+JSON bindings (a2a-rs supports both; neither Swift SDK does yet)
- [ ] Weekly scheduled job bumping pinned SDK revisions, opening an issue on new failures (drift detection)

Out of scope: performance measurement, authenticated/extended-card flows, anything vendor-specific.

## Arkavo extension specs and scenarios

`specs/arkavo/` holds specifications for Arkavo's A2A protocol extensions (WebSocket binding, CBOR codec, identity, policy gating, TDF part encryption, iroh discovery), and `scenarios/arkavo/` their conformance scenarios. They follow the same spec-first discipline as the core corpus: a capability exists when its Rust↔Swift matrix cells are green, not before. Extension scenarios apply only to the `arkavo-ext-*` adapter implementations (plus the mandatory `degradation/` row, which runs vanilla clients against extended servers); they are inert (`n/a`) for the core matrix.

**Governance note:** the donation offer to a2aproject covers the vendor-neutral core of this repository (runner, schemas, core corpus, adapters for public SDKs). `specs/arkavo/` and `scenarios/arkavo/` are Arkavo-stewarded; if donation ever proceeds, they move out to an Arkavo repository first. Two of the extension specs (`ws-binding-v1`, `cbor-codec-v1`) are themselves vendor-neutral upstream candidates and would move *up*, not out.

## License

[Apache 2.0](LICENSE). Vendor-neutral by design; donation to [a2aproject](https://github.com/a2aproject) offered if there's interest (subject to the governance note above).
