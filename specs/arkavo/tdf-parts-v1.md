# TDF Parts v1

**Status:** Draft for review · **Extension URI:** `https://arkavo.social/ext/a2a/tdf-parts/v1`
**Depends on:** core spec §4.6 (part metadata). **Interacts with:** `cbor-codec-v1.md` §4 (byte strings).
**This document is normative.**

Part-level TDF/NanoTDF encryption: individual `Part`s of a Message or
Artifact are ciphertext under OpenTDF data-centric protection, while the A2A
exchange around them stays vanilla. KAS protocol, rewrap, and policy
enforcement are **out of scope** — OpenTDF/KAS specifications govern them;
this document pins only the **on-wire part shapes**, write ordering, and
failure behavior.

## 1. Advertisement

The extension URI appears in `AgentCard.capabilities.extensions` with
`required: false` — always. Encryption is per-part, mixed messages are legal
(§5), so a peer that cannot decrypt still has a meaningful conversation.

`AgentExtension.params` schema (JSON Schema fragment):

```jsonc
{
  "type": "object",
  "properties": {
    "schemes": { "type": "array", "items": { "enum": ["nanotdf", "tdf"] }, "minItems": 1 }, // REQUIRED
    "kas":     { "type": "array", "items": { "type": "string", "format": "uri" } },         // OPTIONAL. KAS URLs this agent encrypts against
    "gateway": { "type": "string", "format": "uri" }                                        // OPTIONAL. b3 artifact gateway base URL (§3)
  },
  "required": ["schemes"]
}
```

> **DECISION (proposed default):** `params.kas` defaults to
> `["https://kas.arkavo.net"]`; `params.gateway` to
> `https://tdf.arkavo.net`. Both are deployment knobs advertised for
> sender convenience, not trust anchors — trust derives from the TDF
> manifest, never from the card.

## 2. Shape (a): inline NanoTDF part

A `data` Part (proto `Part.data`, a structured JSON value) whose value is:

```jsonc
{
  "manifest":   { /* NanoTDF header fields object: kasUrl, ecc binding, policy, ephemeral key — per NanoTDF spec */ },
  "ciphertext": "<base64>"   // JSON codec: base64 text (standard alphabet, padded, RFC 4648 §4)
                              // CBOR codec: byte string (major type 2), transcoding per cbor-codec-v1 §4 rules
}
```

Pinned fields:

| item | pinned value |
|---|---|
| `Part.mediaType` | `application/vnd.arkavo.nanotdf+json` |
| part metadata key | `https://arkavo.social/ext/a2a/tdf-parts/v1#enc` |
| metadata value | `{"scheme": "nanotdf", "v": 1}` |

> **DECISION (proposed default):** mediaType
> `application/vnd.arkavo.nanotdf+json` — the `+json` suffix is honest (the
> part *value* is a JSON structure carrying the binary inside it) and keeps
> vanilla content-type plumbing from treating the part as opaque
> octet-stream. The raw-binary alternative
> (`application/vnd.opentdf.nanotdf` in a `raw` part) is deferred: splitting
> manifest from ciphertext lets receivers route on KAS/policy without
> parsing NanoTDF binary framing.

The `#enc` metadata key is the **normative discriminator**: receivers MUST
key decryption off `#enc`, not off mediaType sniffing. A part with `#enc`
whose value fails to parse per this section is handled as a part-level
failure (§4), code `manifest-invalid`.

The inner mediaType of the *plaintext* (what the part becomes after
decryption) SHOULD be carried in the NanoTDF policy/metadata, not on the
wire part — the wire deliberately does not leak plaintext content type.

## 3. Shape (b): large artifact by reference

Large ciphertexts (RECOMMENDED threshold: anything that would push the
enclosing envelope toward the 16 MiB frame limit of `framing-envelope-v1`
§4; in practice ≥ 1 MiB) travel out-of-band: a `url` Part pointing at the
ciphertext blob, addressed by its BLAKE3 hash.

Pinned fields:

| item | pinned value |
|---|---|
| `Part.url` | `https://<gateway-host>/b3/<blake3-hex>` (64 lowercase hex chars) |
| `Part.mediaType` | `application/vnd.opentdf.tdf` (the blob is a standard TDF archive) |
| part metadata key `…#enc` | `{"scheme": "tdf", "v": 1, "b3": "<blake3-hex>"}` |
| part metadata key `…#manifest` | the TDF manifest object (sidecar), so receivers can evaluate KAS/policy before fetching |

> **DECISION (proposed default):** the URL is an **HTTPS gateway URL** with
> path shape `/b3/<hex>`, not a bare `b3://<hex>` URI. Rationale: vanilla
> clients and standard HTTP stacks can fetch it (degradation contract — a
> `url` part that no resolver resolves is strictly worse), caching/CDN
> layers work unmodified, and content addressing is preserved by carrying
> the hash **redundantly in `#enc.b3`**, which is the integrity anchor: a
> receiver MUST verify BLAKE3(fetched bytes) == `#enc.b3` and treat mismatch
> as a part-level failure (§4, `integrity-failed`) regardless of what host
> served the bytes. The gateway host is deployment-chosen
> (`params.gateway`); the `/b3/<hex>` path shape is pinned.

**Payload-first write ordering (pinned, inherited from the S3
architecture):** the sender MUST complete the ciphertext upload — durably
readable at the pinned URL — **before** sending the A2A message or artifact
update that references it. A receiver MAY fetch immediately on receipt; a
404 race is a sender conformance failure, not a receiver retry obligation
(receivers MAY retry anyway; bounded retry is RECOMMENDED for eventual-read
stores).

## 4. Failure behavior: fail closed, at the part

KAS denial, key-unwrap failure, integrity mismatch, or manifest parse
failure MUST NOT degrade the part to plaintext — there is no plaintext to
degrade to, and implementations MUST NOT substitute cached or re-fetched
plaintext for a part whose decryption was denied.

Equally pinned: the failure is **part-scoped, not exchange-scoped**. The A2A
protocol exchange itself succeeds.

> **DECISION (proposed default):** the receiving SDK yields the part to the
> application with the original ciphertext shape intact plus part metadata
> key `https://arkavo.social/ext/a2a/tdf-parts/v1#error` =
> `{"code": "<code>", "detail": "<optional string>"}`; the application
> decides whether a missing part is fatal to its use case. Codes, pinned:
> `kas-denied`, `unwrap-failed`, `integrity-failed`, `manifest-invalid`,
> `fetch-failed`. The rejected alternative — failing the whole
> `SendMessage`/`GetTask` result — would let one undecryptable part destroy
> an otherwise-useful mixed message and would misreport a *local
> authorization* outcome as a *protocol* failure. `#error` is only ever
> attached by the **receiving** SDK locally; it MUST NOT appear on the wire.

## 5. Mixed messages and degradation

Encrypted and plaintext parts MAY coexist in one Message or Artifact, in any
order. Vanilla peers see encrypted parts as opaque-but-well-formed `data` /
`url` parts with unknown metadata (ignored per core §5.7) and can still read
every plaintext part — that is the degradation contract, and why
`required: false` is mandatory. Senders SHOULD NOT send encrypted parts to
peers whose card does not advertise this extension *when the content is
useless without decryption*; sending anyway is legal (the part is then dead
weight, not an error).

A vanilla peer that fetches a shape-(b) URL receives a TDF archive it cannot
open: confidentiality holds with zero cooperation from the peer, which is
the point of data-centric protection.

## 6. Out of scope

KAS rewrap protocol, attribute/policy semantics, key management, and TDF
archive internals: see the OpenTDF specifications. Identity presented to KAS
is expected to be the `aia-identity-v1` credential where both are deployed,
but neither extension requires the other.

## 7. Conformance

`scenarios/arkavo/tdf/`:

- `nanotdf-part-roundtrip` — shape-(a) part: sender encrypts, mediaType and
  `#enc` exactly as pinned in §2, receiver (holding the key, scripted KAS)
  decrypts to byte-identical plaintext; under WS/CBOR the `ciphertext` field
  is major type 2 on the wire.
- `tdf-part-wrong-key-fails-closed` — receiver's unwrap is denied (scripted
  KAS denial): A2A exchange completes successfully, part surfaces with
  `#error.code = "kas-denied"`, plaintext appears nowhere in the receiving
  SDK's decoded view, sibling plaintext parts in the same message are
  delivered normally.
- `b3-url-artifact-integrity` — shape-(b) part: upload completes before the
  referencing message (wire capture ordering check), URL matches the pinned
  `/b3/<hex>` shape, fetch + BLAKE3 verification succeeds; mutated-blob leg
  ⇒ `#error.code = "integrity-failed"`, fail-closed.
