# CBOR Wire Codec v1

**Status:** Draft for review · **Extension URI:** `https://arkavo.social/ext/a2a/cbor-codec/v1`
**Upstream candidate:** yes — the `WireCodec` abstraction and this mapping are
vendor-neutral content negotiation, intended as PRs to `a2a-swift` and `a2a-rs`
once proven in the matrix (evidence-first; see plan §8).
**This document is normative.**

CBOR (RFC 8949) as an alternative wire encoding for A2A JSON-RPC traffic.
Motivation: CWT/COSE credentials (`aia-identity-v1`), NanoTDF (`tdf-parts-v1`),
and the wire itself become one CBOR/COSE binary stack for constrained targets
(watchOS companions, Pi mesh nodes), and raw `Part` bytes ship without base64
overhead.

## 1. WireCodec abstraction

Both SDKs gain a codec seam. Swift surface (Rust trait mirrors it):

```swift
public protocol WireCodec: Sendable {
    var contentType: String { get }            // "application/json" | "application/cbor"
    var subprotocolSuffix: String { get }      // "json" | "cbor"
    func encode<T: Encodable>(_ value: T) throws -> Data
    func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T
}
```

Dispatcher and client take a codec; **default is JSON with zero behavior
change**. HTTP binding negotiates via `Content-Type`/`Accept:
application/cbor`; the WS binding negotiates via subprotocol
(`ws-binding-v1.md` §2).

## 2. Deterministic encoding (normative)

Encoders MUST emit RFC 8949 §4.2.1 **Core Deterministic Encoding**:

1. Integers and lengths in shortest form.
2. **Definite lengths only** — no indefinite-length items.
3. Map keys sorted by the bytewise lexicographic order of their *encoded* form.
4. No duplicate map keys; decoders MUST reject duplicates.

> **Implementation warning (the reason this section exists):** neither
> `ciborium` (Rust) nor any current Swift CBOR library produces canonical map
> ordering by default — serde/Codable emit struct fields in declaration order.
> Implementations MUST apply an explicit canonicalization pass (encode →
> generic CBOR value → recursive key sort → serialize) or an encoder
> implementing the ordering natively. **The spec, not the library default, is
> normative.** Cross-language byte-identity is enforced by `scenarios/arkavo/wire/`.

Decoders MUST accept any well-formed CBOR (liberal in what you accept) but
MUST NOT emit non-deterministic form.

## 3. JSON ↔ CBOR mapping

The CBOR document model is the JSON model with these refinements:

| JSON | CBOR | rule |
|---|---|---|
| object | map (text keys) | keys are text strings; ordering per §2.3 |
| array | array | — |
| string | text string | — |
| `true`/`false`/`null` | simple values 21/20/22 | — |
| number (integer-valued) | major type 0/1 integer | integers MUST NOT be encoded as floats |
| number (fractional) | float, shortest width preserving the value exactly | half → single → double |
| **`Part.raw` base64 string** | **byte string (major type 2)** | see §4 |

No CBOR tags in v1. Enum strings (`TASK_STATE_*`, `ROLE_*`, `kind` values)
remain **text strings** — an integer registry is a v2 optimization deliberately
deferred to avoid fragmenting the ecosystem now.

## 4. Raw parts as byte strings

In CBOR form, the `raw` field of a `Part` is a **byte string** containing the
literal bytes — this is the size win. Transcoding is lossless and direction-
deterministic:

- JSON → CBOR: base64-decode the `raw` text string → byte string.
- CBOR → JSON: base64-encode (standard alphabet, padded, RFC 4648 §4) → text string.

The same rule applies to every field the core spec defines as proto `bytes`
(currently only `Part.raw`). All other strings stay text strings even if they
look base64-ish (`AgentCardSignature.protected`/`signature` are base64url
*text* per RFC 7515 — they are NOT transcoded).

## 5. Negotiation & degradation

- The extension URI in `AgentCard.capabilities.extensions` advertises CBOR
  support; `required` MUST be `false` (a `required:true` CBOR-only agent would
  reject all vanilla peers with −32008 — permitted by A2A but pathological;
  the degradation row tests the `required:false` contract).
- A server MUST keep serving plain JSON to peers that don't negotiate CBOR
  (`scenarios/arkavo/wire/content-negotiation-fallback-to-json`,
  `scenarios/arkavo/degradation/*`).

## 6. Conformance

`scenarios/arkavo/wire/`:
- `cbor-roundtrip-all-golden-vectors` — every golden vector in the core corpus
  round-trips JSON → typed model → CBOR → typed model → JSON byte-identically,
  and the canonical CBOR bytes are identical across implementations.
- `cbor-raw-part-bytestring` — `Part.raw` is major type 2 on the wire and
  transcodes per §4.
- `content-negotiation-fallback-to-json` — un-negotiated peers get JSON.
