# Iroh Discovery & Transport v1

**Status:** Draft for review (Phase 6 — interface shape pinned now so cards can advertise it early; implementation later)
**Extension URI:** `https://arkavo.social/ext/a2a/iroh-discovery/v1`
**Depends on:** `framing-envelope-v1.md`, `cbor-codec-v1.md`.
**This document is normative.**

A2A over iroh: NAT-traversing, relay-assisted QUIC connections addressed by
node ID instead of DNS. Cards advertise an iroh interface; peers resolve a
card *from* a node ID; traffic reuses the framing envelope verbatim. A
relay HTTPS gateway gives Swift (and any vanilla) clients a plain-HTTP path
until native iroh bindings exist (Phase-1 fallback, §5).

## 1. Advertisement

An additional `AgentInterface` entry (the standard `"JSONRPC"` HTTPS
interface MUST still be listed **first**, as in `ws-binding-v1` §1):

```jsonc
{
  "url": "iroh://<node-id>",          // node-id: z-base-32 iroh NodeId, lowercase
  "protocolBinding": "JSONRPC-IROH",  // open-form binding per core §5.8
  "protocolVersion": "1.0"
}
```

The extension URI appears in `capabilities.extensions` with
`required: false` — always.

`AgentExtension.params` schema (JSON Schema fragment):

```jsonc
{
  "type": "object",
  "properties": {
    "nodeId":  { "type": "string" },                    // REQUIRED. z-base-32 NodeId, = the iroh:// authority
    "relays":  { "type": "array", "items": { "type": "string", "format": "uri" } }, // OPTIONAL. iroh relay URLs
    "gateway": { "type": "string", "format": "uri" }    // OPTIONAL. HTTPS fallback gateway base (§5)
  },
  "required": ["nodeId"]
}
```

> **DECISION (proposed default):** `gateway` defaults to
> `https://iroh.arkavo.net`. `nodeId` is duplicated out of the URL so
> consumers that index cards by extension params need not parse `iroh://`
> URLs.

## 2. ALPN and codec negotiation

iroh connections carry no subprotocol header; **ALPN is the negotiation
surface**, covering both protocol and codec in one string:

| ALPN | meaning |
|---|---|
| `arkavo/a2a/1` | this binding, envelope + payloads in JSON |
| `arkavo/a2a-cbor/1` | this binding, envelope + payloads in CBOR per `cbor-codec-v1.md` |

> **DECISION (proposed default):** ALPN-based codec selection. The
> alternatives — an in-band negotiation frame (a new envelope kind, rejected
> by `framing-envelope-v1`'s closed kind set) or per-frame content sniffing
> (the drift generator this suite exists to kill) — both cost more than a
> second ALPN string. Clients offer both ALPNs in preference order; the
> QUIC handshake selects exactly one, mirroring `ws-binding-v1` §2
> subprotocol semantics. A node supporting neither offered ALPN fails the
> handshake; there is no JSON fallback *inside* a connection.

Version bumps of the binding mint new ALPN strings (`arkavo/a2a/2`); the
trailing `/1` is the binding version, independent of the envelope's `v`.

## 3. Transport mapping

One iroh **bidirectional stream** per connection carries all traffic, both
directions, multiplexed by envelope `id` exactly as in `ws-binding-v1` §3
(unary `req`→`res`, streaming `req`→`ev`*→`fin`, re-subscribe on the same
stream with a fresh id).

iroh streams are byte streams, not message streams, so framing is explicit:

- Each envelope frame is preceded by a **u32 big-endian length prefix**
  counting the encoded envelope bytes (prefix excluded).
- Max frame size remains **16 MiB** per `framing-envelope-v1` §4 — a length
  prefix > 16 777 216 is envelope error −33001 and the receiver MAY reset
  the stream (the u32 ceiling is not a license to send 4 GiB frames).
- Stream reset ≙ connection close for lifecycle purposes
  (`framing-envelope-v1` §6): all open ids surface as transport errors,
  never clean completion.
- Keep-alive is QUIC's (iroh's) job, per `framing-envelope-v1` §4.

Service parameters (`A2A-Version`, `A2A-Extensions`) have no header channel
here; they ride the `arkavo/ResolveCard` exchange (§4) request `params` as
`{"a2aVersion": "...", "extensions": [...]}` and apply to the whole
connection, mirroring `ws-binding-v1` §3's connection-scoped semantics.

## 4. Card resolution by node ID

Bootstrapping inverts the usual order: the dialer knows a NodeId but has no
card yet (no URL to fetch `/.well-known/agent-card.json` from). Resolution
is in-band:

1. Connect to the node with ALPN `arkavo/a2a/1` (or `-cbor`).
2. Open the bidirectional stream; send envelope `req` whose payload is the
   JSON-RPC request `{"jsonrpc":"2.0", "id":1, "method":"arkavo/ResolveCard", "params":{}}`.
3. The node answers `res` whose `result` is its full `AgentCard`.

> **DECISION (proposed default):** a dedicated, namespaced custom method
> **`arkavo/ResolveCard`** rather than reusing `GetExtendedAgentCard`.
> Reuse would be false economy: `GetExtendedAgentCard` is defined as an
> *authenticated, capability-gated* refinement of an already-fetched public
> card (−32007 semantics and all), while this is the anonymous public-card
> bootstrap — conflating them would force every iroh dial through extended-
> card error paths and imply `extendedAgentCard: true` on agents that have
> no extended card. A custom method in the extension's namespace rides the
> existing envelope machinery with zero new frame kinds, and is invisible
> to peers that never dial `iroh://`. The card returned MUST be the same
> document served at the agent's well-known HTTPS path (byte-equal after
> JSON canonicalization), signatures included (`aia-identity-v1` §6).

`arkavo/ResolveCard` MUST be served on this binding; it MUST NOT be required
on HTTP bindings (where the well-known path already exists). Unknown methods
on this binding map to −32601 as usual.

## 5. Phase-1 Swift fallback: relay HTTPS gateway

Until Swift has native iroh bindings, a deployment-operated gateway proxies
HTTPS to iroh. Pinned URL shape:

```
https://iroh.arkavo.net/<node-id>/                                  → base for the proxied agent
https://iroh.arkavo.net/<node-id>/.well-known/agent-card.json       → the agent's card
https://iroh.arkavo.net/<node-id>/                                  → standard JSONRPC binding (POST), proxied
```

The proxied JSON-RPC endpoint is the **node base** `…/<node-id>/`: the gateway
accepts the JSON-RPC POST at that path and proxies it over iroh. A relay
client constructs its op URL from the `<gateway>/<node-id>/` base it resolved
the card through — it MUST NOT depend on parsing a JSON-RPC sub-path out of the
card (the card's interface URLs describe the *native* binding). When an agent
lists a gateway-proxied HTTPS interface first (§5 last paragraph), that
interface's `url` is this same node base, so both routes converge.

`<node-id>` is the same z-base-32 string as the `iroh://` authority. The
gateway terminates TLS, dials the node over iroh (ALPN of its choice), and
proxies the standard **JSONRPC HTTP binding** — so a Swift client speaks
plain HTTP+SSE against the gateway URL with zero iroh awareness. The card
served through the gateway MUST be identical to the §4-resolved card.
End-to-end content confidentiality through the (trusted-infrastructure but
still intermediary) gateway is `tdf-parts-v1`'s job, not TLS's.

Agents advertising this extension SHOULD list the gateway-proxied HTTPS URL
as their *first* interface entry while the fallback phase lasts — that is
what makes the degradation contract real for clients that can't parse
`iroh://`.

## 6. Degradation

Vanilla peers select the first supported interface per core §8.3.2 and never
touch `iroh://` (an unknown URL scheme on a later interface entry is legal
card content; finding 6 in FINDINGS.md shows why first-position discipline
matters in practice). The gateway path (§5) means even iroh-only-reachable
agents present a fully vanilla HTTPS face. `required` is always `false`.

## 7. Conformance

`scenarios/arkavo/discovery/`:

- `card-by-node-id` — dial by NodeId with ALPN `arkavo/a2a/1`,
  `arkavo/ResolveCard` returns a card that matches the well-known-path card
  after canonicalization; repeat under `arkavo/a2a-cbor/1` with
  byte-identical decoded result.
- `relay-fallback-equivalence` — the same scenario run natively
  (iroh, §3–§4) and through the gateway (§5) yields byte-identical decoded
  results, including the card.

Partial matrix acknowledged for Phase 6: native legs run rs↔rs only
(iroh bindings are Rust-first); Swift participates via the relay gateway
(`swift-via-relay`). Cells that cannot run a leg report `skip` at selection
time per the honest-cells rule, never `fail`.Connection-level authentication follows the credential-vs-connection
lifetime rule pinned in `ws-binding-v1.md` §3 verbatim: validated at
connection establishment, independent of token TTL thereafter, optional
server-imposed maximum lifetime advertised in extension params.


