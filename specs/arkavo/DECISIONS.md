# Resolved decision points

## §7.3 — Linux URLSessionWebSocketTask (resolved 2026-06-11, Phase 4)

**Decision: use `URLSessionWebSocketTask`; no NIO fallback.**

Source inspection of swift-corelibs-foundation `release/6.0` (the swift:6.3
Linux base): `URLSessionWebSocketTask` is a complete implementation —
`send`/`receive`/`sendPing`/`cancel` are all functional, with real frame
encoding via `doPendingWork()`. It is explicitly NOT in the
`@available(*, unavailable) + NSUnsupported()` category that
`URLSessionStreamTask` sits in. This is unlike the `URLSession.bytes`
landmine (compiled but runtime-broken on Linux), so the §7.3 risk does not
materialize.

**Verification, not assumption:** "compiles on Linux" is not "works on
Linux" (the bytes lesson). The runtime proof is the `arkavo/ws/*` and
`arkavo/transport-equivalence/*` matrix cells running with the Swift ext
client over WS against the Rust ext server, executed in the Linux CI leg.
Phase 4 is not done until those cells are green on Linux. If the runtime
check surprises us, the contingency remains a NIO-based WS client confined
to the `ArkavoA2AWS` target (the SDK seam is transport-abstracted, so the
fallback would not touch consumers) — but the source evidence makes this
unlikely.

## §7.4 — iroh from Swift (resolved 2026-06-11, Phase 6)

**Decision: Rust gets native iroh; Swift uses the relay-HTTPS gateway; FFI deferred.**

Pre-decided by the plan (§6 build order, §7.4): native iroh bindings are
Rust-first; Swift participates via the relay HTTPS gateway
(`iroh-discovery-v1.md` §5) until UniFFI/C-FFI bindings are justified by real
Fluxial/Muse latency needs. The Phase-1 relay path keeps Swift apps shipping
with zero iroh awareness — they speak plain HTTP+SSE against
`https://<gateway>/<node-id>/…`, which a Rust gateway proxies to the iroh
node. The conformance matrix is therefore **partial** for Phase 6: native
legs are rs↔rs; Swift runs `swift-via-relay`. Cells that can't run a leg
report `skip` (honest-cells rule), never `fail`.

**Hermetic testing:** iroh nodes connect by explicit `NodeAddr` (NodeId +
direct `127.0.0.1` socket addresses) — no DNS/relay/internet discovery — so
the conformance harness and CI run fully offline, the same way every other
phase binds ephemeral local ports.
