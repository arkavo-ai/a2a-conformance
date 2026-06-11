# WebSocket harness behavior contract (Phase 4)

How the `arkavo-ext-*` adapters realize `scenarios/arkavo/ws/*` and
`scenarios/arkavo/transport-equivalence/*` against `ws-binding-v1.md`,
`framing-envelope-v1.md`, and `cbor-codec-v1.md`. Supplements CONTRACT.md.

## Server: dual interface

The ext server harness always binds its existing HTTP/JSONRPC endpoint AND a
WebSocket endpoint (Rust: `A2AWsServer::serve` over the same scripted
`RequestHandler`; Swift: an `A2AWsServer`-equivalent over the scripted handler
— if the Swift WS layer is client-only this phase, the WS *server* role is
filled by the Rust ext server, and Swift-server WS cells report `skip` with
that reason, documented; the gate is Swift-client↔Rust-server + the
self/equivalence cells that don't need a Swift WS server).

The served agent card advertises both interfaces, JSONRPC first (degradation):

```jsonc
"supportedInterfaces": [
  {"url": "http://127.0.0.1:<p>/",        "protocolBinding": "JSONRPC",    "protocolVersion": "1.0"},
  {"url": "ws://127.0.0.1:<wsp>/",        "protocolBinding": "JSONRPC-WS", "protocolVersion": "1.0"}
]
```

The WS port is reported in the `READY` line as an added field `wsBaseUrl`, or
discovered by the client from the card's `JSONRPC-WS` interface (preferred —
exercises real interface selection).

## Client: transport selection by scenario

| scenario group | client transport |
|---|---|
| `arkavo/ws/*` | connect via the SDK's WS client to the card's `JSONRPC-WS` interface; codec offer per the scenario (see below); drive the op; emit the same outcome shape as the HTTP path (result/error/stream). |
| `arkavo/transport-equivalence/*` | run the op **twice** — once over HTTP/JSONRPC, once over WS — and assert the decoded results are byte-identical *in the harness*; emit `result`/`stream` from the WS run (the HTTP run is the equivalence oracle). For `send-message-equivalent`, also run WS/CBOR as a third leg when both peers negotiate it. |
| everything else | HTTP, exactly as today. |

Codec negotiation for `ws-subprotocol-negotiation`: the harness offers
`[a2a.v1.cbor, a2a.v1.json]` — **both** subprotocols in preference order, so the
cell genuinely exercises preference-order negotiation — and asserts the server
selects `a2a.v1.cbor` (the SDK exposes the accepted subprotocol/codec — Rust
`A2AWsClient::format()`, Swift the connection's negotiated codec). Other ws
cells also offer `[a2a.v1.cbor, a2a.v1.json]` so the canonical CBOR binary-frame
path is exercised wherever the server negotiates it.

## Connection lifecycle (per-op)

The runner has no `/select` signal for the client harness — it sends one op line
per scenario, and every multi-step WS behaviour
(`ws-concurrent-streams-multiplexed`, `ws-resubscribe-same-socket`) runs
**within a single op-line handler** on one live socket. So the harness opens the
WS connection inside the scenario's op handler and tears it down at the end of
that handler (per-op lifecycle). "Same socket" therefore means *within one op
handler*: re-subscribe reuses the live connection with a fresh id, and the two
concurrent streams multiplex over the one open connection, before it is closed.
There is no cross-op connection cache; per-op open/close honours "torn down"
deterministically and removes any stale-connection risk.

## §7.3 runtime proof

These cells running green on the **Linux** CI leg (Swift ext client built on
`swift:6.3` corelibs, WS over `URLSessionWebSocketTask`, against the Rust ext
WS server) is the empirical resolution of decision §7.3 — not the source
inspection, which only told us it would compile. Until that leg is green,
§7.3 is "resolved pending CI proof" (DECISIONS.md).
