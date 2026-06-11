# Framing Envelope v1

**Status:** Draft for review · **Consumed by:** `ws-binding-v1`, `iroh-discovery-v1`
**This document is normative.** The words MUST/SHOULD/MAY follow RFC 2119.

A bidirectional, multiplexing frame format carrying A2A JSON-RPC traffic over
message-oriented transports (WebSocket messages, iroh stream chunks). One
envelope per transport message. The envelope is codec-agnostic: the *frame
structure* is fixed; the encoding of the frame (JSON or CBOR) is negotiated by
the enclosing transport per `cbor-codec-v1.md`.

## 1. Frame structure

```jsonc
{
  "v": 1,            // envelope version. MUST be 1. Unknown versions: close, code 1002-equivalent (§6).
  "id": "r-42",      // correlation id. ALWAYS a string (§2).
  "kind": "req",     // "req" | "res" | "ev" | "fin" | "err" (§3)
  "payload": { … }   // per-kind body, encoded with the negotiated codec. Absent for "fin".
}
```

Field names are identical in JSON and CBOR form. CBOR uses **text-string keys**
(`"v"`, `"id"`, `"kind"`, `"payload"`), not integer keys — readability and a
single mapping table over a ~6-byte saving. Unknown envelope fields MUST be
ignored (forward compatibility, mirroring A2A §5.7).

## 2. Correlation id

- `id` is ALWAYS a string at the envelope layer. A JSON-RPC request whose `id`
  is a number is normalized: envelope `id` = the decimal string rendering; the
  *payload* retains the caller's original JSON-RPC `id` untouched. This pins
  away the int-vs-string drift observed between SDKs.
- Initiator-chosen ids MUST be unique among that initiator's in-flight
  exchanges on the connection. Reuse after `fin`/terminal `res` is permitted
  but NOT RECOMMENDED.
- Multiple ids MAY be in flight concurrently on one connection (that is the
  point of the envelope).

## 3. Kinds

| kind | direction | payload | semantics |
|---|---|---|---|
| `req` | client → server | one JSON-RPC 2.0 request object | starts an exchange under `id`. Unary or streaming is determined by the JSON-RPC method (`A2AMethod.streaming`). |
| `res` | server → client | one JSON-RPC 2.0 response object (result **or** error) | terminal for a unary exchange. A JSON-RPC *error* to a streaming request is also delivered as `res` (terminal) — this resolves, for envelope transports, the ambiguity noted in FINDINGS.md (§9.4.2 spec gap). |
| `ev` | server → client | one JSON-RPC 2.0 response object whose `result` is a `StreamResponse` | one stream event for `id`. MUST NOT follow `fin`/`res` for the same `id`. |
| `fin` | server → client | absent | terminal for a streaming exchange; maps to SSE stream close. MANDATORY: every streaming exchange ends with exactly one `fin` (after zero or more `ev`) unless terminated by `res`-error. |
| `err` | either direction | `{"code": int, "message": string}` | transport-level failure for `id` (or connection-level if `id` is `""`): malformed frame, oversize, unknown kind. Distinct from JSON-RPC errors, which ride in `res`. Terminal for the `id` it names. |

Ordering: the server MUST NOT reorder frames *within* one `id` (transport
ordering is relied upon; both WS and iroh streams provide it). No ordering is
guaranteed *across* ids.

## 4. Limits

- Max frame size: **16 MiB** (encoded envelope). A peer receiving an oversize
  frame MUST reply `err` `{code: -33001, message: "frame too large"}` for that
  id — or with `id: ""` when the oversize frame's id is unknowable without
  decoding it — and MAY close the connection. Senders SHOULD chunk large artifacts via
  `TaskArtifactUpdateEvent.append` instead of approaching the limit, or use
  `b3/<hash>` URL parts (`tdf-parts-v1.md`).
- Keep-alive is the transport's job (WS ping/pong frames; iroh keepalive) —
  never an envelope kind.

## 5. Transport error codes (`err.code`)

Negative range −33000…−33099, disjoint from JSON-RPC and A2A §5.4 codes:

| code | meaning |
|---|---|
| −33000 | malformed envelope (undecodable, missing required field, bad `v`) |
| −33001 | frame too large |
| −33002 | unknown `kind` |
| −33003 | `ev`/`fin`/`res` for an id with no open exchange (receivers SHOULD drop the frame and MAY report this code) |
| −33004 | codec mismatch (payload not decodable under the negotiated codec) |

## 6. Connection lifecycle

- Envelope version mismatch at first frame: peer sends `err` −33000 with
  `id: ""` and closes (WS close code 1002 / iroh stream reset).
- Half-open exchanges at connection close are implicitly terminated; clients
  MUST surface them as transport errors, never as clean stream completion
  (the silent-empty-stream defect class — FINDINGS.md findings 3 and 9).

## 7. Conformance

Validated by `scenarios/arkavo/ws/*` and `scenarios/arkavo/transport-equivalence/*`:
multiplexed concurrent streams, in-order delivery per id, mandatory `fin`,
oversize behavior, and byte-identical decoded results across HTTP/JSON ≡
WS/JSON ≡ WS/CBOR for the same scenario (precedent: a2a-itk transport
equivalence).
