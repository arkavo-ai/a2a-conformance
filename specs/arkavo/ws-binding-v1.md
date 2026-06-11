# WebSocket Binding v1

**Status:** Draft for review · **Extension URI:** `https://arkavo.social/ext/a2a/ws-binding/v1`
**Upstream candidate:** yes. **Depends on:** `framing-envelope-v1.md`, `cbor-codec-v1.md`.
**This document is normative.**

A2A JSON-RPC over a single WebSocket connection using the framing envelope.
Replaces the request-per-HTTP-exchange model with one multiplexed, long-lived,
bidirectionally-keepalive connection — the natural shape for companion apps
holding open subscriptions.

## 1. Advertisement

An additional `AgentInterface` entry with open-form `protocolBinding`
`"JSONRPC-WS"` (A2A §5.8 permits open-form bindings):

```jsonc
{
  "url": "wss://agent.example.com/a2a/ws",
  "protocolBinding": "JSONRPC-WS",
  "protocolVersion": "1.0"
}
```

The standard `"JSONRPC"` HTTPS interface MUST always be listed **first** —
vanilla clients never see a difference (degradation contract). The extension
URI also appears in `capabilities.extensions` with `required: false`.

## 2. Subprotocol negotiation

Codec selection rides `Sec-WebSocket-Protocol`:

- `a2a.v1.json` — envelope and payloads encoded as JSON (UTF-8, WS text frames)
- `a2a.v1.cbor` — envelope and payloads encoded as CBOR per `cbor-codec-v1.md`
  (WS binary frames)

Clients offer one or both (preference order); the server selects exactly one
and echoes it. If the server supports neither offered subprotocol it MUST fail
the handshake. A client offering nothing gets `a2a.v1.json`. Frame type
mismatch (text frame under cbor, binary under json) is envelope error −33004.

## 3. Mapping to A2A operations

| A2A operation | over the envelope |
|---|---|
| unary methods (`SendMessage`, `GetTask`, …) | `req` → `res` under one id |
| `SendStreamingMessage` | `req` → `ev`* → `fin` (or terminal `res` carrying the JSON-RPC error) |
| `SubscribeToTask` | same as streaming send; **re-subscription reuses the same socket** with a fresh id (`ws/ws-resubscribe-same-socket`) |
| service parameters (`A2A-Version`, `A2A-Extensions`) | HTTP headers on the WS upgrade request, exactly as in §9.2; they apply to the whole connection |

Authentication (`aia-identity-v1` or any `RequestAuthenticator`) applies to
the upgrade request; per-message re-authentication is not part of v1.

## 4. Connection semantics

- Server-initiated close mid-exchange ⇒ clients surface transport errors for
  all open ids (never clean completion) — `framing-envelope-v1.md` §6.
- Push-notification configs are unnecessary in-connection: `ev` covers the
  webhook use case while the socket lives (plan §8 keeps WS webhooks out of scope).
- Reconnection/backoff policy is client-discretionary in v1; `SubscribeToTask`
  after reconnect is the recovery mechanism (tasks are the durable unit).

## 5. Conformance

`scenarios/arkavo/ws/`: `ws-send-message`, `ws-concurrent-streams-multiplexed`
(two streaming exchanges interleaved on one socket, per-id order preserved),
`ws-resubscribe-same-socket`, `ws-subprotocol-negotiation` (json preference,
cbor preference, unsupported-only offer ⇒ handshake failure).
`scenarios/arkavo/transport-equivalence/`: decoded results for the same
scenario byte-identical across HTTP/JSON ≡ WS/JSON ≡ WS/CBOR.
