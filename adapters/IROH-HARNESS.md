# Iroh discovery harness behavior contract (Phase 6)

How the `arkavo-ext-*` adapters realize `scenarios/arkavo/discovery/*` against
`iroh-discovery-v1.md`. Partial matrix by design (§7.4 / spec §7): native legs
are rs↔rs; Swift runs only the relay path.

## Topology (hermetic — no DNS/relay/internet)

- The **Rust ext server** additionally starts an iroh `Endpoint` bound to a
  loopback socket, accepting ALPNs `arkavo/a2a/1` and `arkavo/a2a-cbor/1`,
  serving the same scripted handler + `arkavo/ResolveCard`. Its node address (NodeId + `127.0.0.1:<port>` direct address) is reported in
  the `READY` line as `irohNodeAddr`, serialized as
  `<z-base-32-node-id>@<ip:port>,<ip:port>…` (the dialable form; a bare
  `iroh://<node-id>` carries no direct addresses and is un-dialable under the
  no-discovery hermetic topology — discovery would supply them in production).
- The **Rust ext server** also starts a **relay gateway**: a plain HTTP server
  that, for `GET/POST /<node-id>/…`, dials the iroh node (using the known
  NodeAddr) and proxies the standard JSONRPC HTTP binding + the well-known card
  path (spec §5). Its base URL is reported as `irohGatewayBaseUrl`.
- The served card advertises (JSONRPC HTTP first — degradation): the
  iroh interface `{url:"iroh://<nodeId>", protocolBinding:"JSONRPC-IROH",
  protocolVersion:"1.0"}` and the iroh-discovery extension
  `{uri, required:false, params:{nodeId, gateway:"<irohGatewayBaseUrl>"}}`.

## Client transport selection by impl + scenario

| client | discovery/* behavior |
|---|---|
| `arkavo-ext-rust` | **native iroh**: read the card's iroh interface (or the READY `irohNodeAddr`), dial by NodeId with ALPN per the scenario codec, `arkavo/ResolveCard`, drive the op over the iroh bi-stream (framing envelope, u32-BE length prefix). |
| `arkavo-ext-swift` | **relay only**: construct `<gateway>/<nodeId>/.well-known/agent-card.json`, resolve the card over plain HTTPS, then drive the op via the normal HTTP `A2AClient` against `<gateway>/<nodeId>/`. No iroh awareness. |

Server side: `arkavo-ext-swift` has **no iroh server** (Rust-first), so any
`discovery/*` cell with `server=arkavo-ext-swift` reports `skip` at `/select`
(reason: "Swift has no iroh server; native iroh is Rust-first") — the gate is
rs↔rs native + swift-client→rust-server-via-relay.

## Scenarios

| scenario | behavior |
|---|---|
| `card-by-node-id` | resolve the card by NodeId (native: `arkavo/ResolveCard` over iroh under both ALPNs; relay: gateway well-known path). Assert the resolved card matches the well-known-path card after JCS canonicalization; under the cbor ALPN the decoded result is byte-identical. |
| `relay-fallback-equivalence` | run the same op natively (rust client → iroh) AND through the gateway (any client → relay → iroh), assert byte-identical decoded results incl. the card. Rust ext runs both legs and compares in-harness; Swift ext runs the relay leg only and compares against the manifest/native oracle the Rust self-pair already established. |

## Matrix expectations

- `client=arkavo-ext-rust, server=arkavo-ext-rust`: both discovery scenarios pass (native + relay legs).
- `client=arkavo-ext-swift, server=arkavo-ext-rust`: pass via relay (the Swift-via-relay leg).
- `server=arkavo-ext-swift` (any client): discovery scenarios `skip`.
This is the acknowledged partial matrix (spec §7).
