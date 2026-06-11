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
