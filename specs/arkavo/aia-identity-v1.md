# AIA Identity v1

**Status:** Draft for review · **Extension URI:** `https://arkavo.social/ext/a2a/aia-identity/v1`
**Depends on:** core spec §7 (auth), §8.4 (card signing). **Interacts with:** `ws-binding-v1.md` §3 (upgrade-request auth).
**This document is normative.**

CWT (RFC 8392) / COSE credential presentation between A2A agents, with
DID-bound Agent Cards. The Arkavo Identity Authority (AIA) at
`identity.arkavo.net` issues short-lived CWTs that agents present on every
request; Agent Cards are signed with keys resolvable from the agent's DID.
CWT/COSE rather than JWT/JOSE because the wire stack (`cbor-codec-v1`,
NanoTDF in `tdf-parts-v1`) is already CBOR/COSE — one binary credential
format, CryptoKit-native on Apple platforms, `ring`/`p256` + `coset` in Rust.

## 1. Advertisement

The extension URI appears in `AgentCard.capabilities.extensions`. `required`
MAY be `true` for agents that refuse anonymous peers (this is the one Arkavo
extension where `required: true` is a legitimate posture, enforced at the
HTTP layer per §7 — not via −32008, which covers only the client's failure to
*declare* the extension).

`AgentExtension.params` schema (JSON Schema fragment):

```jsonc
{
  "type": "object",
  "properties": {
    "issuer":  { "type": "string", "format": "uri" },   // REQUIRED. The trusted AIA issuer.
    "did":     { "type": "string", "pattern": "^did:" }, // REQUIRED. This agent's DID (the aud peers must target).
    "algs":    { "type": "array", "items": { "const": "ES256" } }, // OPTIONAL. v1: ["ES256"] only.
    "maxTtl":  { "type": "integer", "maximum": 300 }     // OPTIONAL. Max accepted CWT lifetime, seconds.
  },
  "required": ["issuer", "did"]
}
```

> **DECISION (proposed default):** `issuer` = `https://identity.arkavo.net`,
> `maxTtl` = `300`. Both are deployment knobs; these are the Arkavo defaults.

## 2. CWT claim set

The credential is a CWT carried as a `COSE_Sign1` structure. Claims, all by
their registered integer keys:

| claim | key | value | requirement |
|---|---|---|---|
| `iss` | 1 | the AIA issuer URI (text string) | REQUIRED; MUST equal the verifier's configured `params.issuer` |
| `sub` | 2 | presenting agent's DID | REQUIRED |
| `aud` | 3 | target agent's DID | REQUIRED; see audience rule below |
| `exp` | 4 | expiry, NumericDate | REQUIRED; `exp − iat` MUST be ≤ 300 s (§5) |
| `iat` | 6 | issued-at, NumericDate | REQUIRED |
| `cti` | 7 | unique token id (byte string, ≥ 16 random bytes) | REQUIRED |
| `cnf` | 8 | RFC 8747 confirmation; `COSE_Key` (key 1) holding the agent's P-256 public key. Carried in v1, **enforced in v2 PoP** — slot reserved so v2 renumbers nothing | REQUIRED |

Verifiers MUST reject tokens missing any required claim, expired tokens
(±30 s clock skew permitted), and tokens whose `aud` does not identify the
verifier. `nbf` (5) MAY be present and MUST be honored if present.

**Audience rule.**

> **DECISION (proposed default):** `aud` is the target agent's **DID**
> (`params.did` from its card). Fallback: when the target's card declares no
> DID (vanilla peer, or extension params absent), `aud` is the target's
> canonical Agent Card URL — `https://{host}/.well-known/agent-card.json`,
> exactly as fetched, no trailing-slash normalization. A verifier MUST accept
> either form that identifies itself.

## 3. COSE profile

- Structure: `COSE_Sign1` (tag 18) over the CWT claims map.
- `alg`: **ES256 (−7) only** in v1. Verifiers MUST reject any other `alg`.
  EdDSA is deliberately deferred: ES256 is CryptoKit-native on Apple
  platforms (including Secure Enclave keys) and first-class in `ring`/`p256`;
  one algorithm means no negotiation surface.
- Protected header MUST contain `alg`; SHOULD contain `kid` = the AIA signing
  key identifier. The unprotected header MUST be ignored for verification
  decisions.
- The CWT MUST be signed by an AIA key. AIA key discovery/rotation is the
  issuer's concern (out of scope here; see §6 for *card* keys, which are the
  agent's own).

## 4. Presentation

The serialized `COSE_Sign1` CWT is presented on **every** A2A HTTP request:

```
Authorization: Bearer <base64url(COSE_Sign1 bytes, unpadded)>
```

> **DECISION (proposed default):** `Authorization: Bearer` rather than a
> dedicated `X-Arkavo-CWT` header. A bearer credential in `Authorization`
> traverses proxies, CDN auth hooks, and both SDKs' existing
> `RequestAuthenticator`-style middleware without new plumbing, and it gets
> the standard RFC 6750 401-challenge machinery for free. The cost — a
> verifier must sniff CWT-vs-JWT — is trivial: a COSE_Sign1 starts with CBOR
> bytes, never with the `eyJ` of a JWT. A custom header would also be the
> first thing stripped by conservative middleware. RFC 6750 `b64token`
> syntax admits base64url, so this is wire-legal.

For `ws-binding-v1`, the header rides the **WS upgrade request** and
authenticates the whole connection. Credential lifetime and connection
lifetime are decoupled per the pinned rule in `ws-binding-v1.md` §3:
validation happens once at upgrade, the connection's validity is independent
of token TTL thereafter, and any server-imposed maximum connection lifetime
is advertised in the ws-binding extension params. The same rule applies to
iroh connections (`iroh-discovery-v1.md`).

## 5. Proof of possession

> **DECISION (proposed default):** PoP enforcement is **punted to v2**. In
> v1, `cnf` is carried (so tokens are PoP-*capable* and v2 needs no claim
> change) but verifiers do not demand a possession proof. Replay risk is
> bounded instead by: `exp − iat` ≤ 300 s (REQUIRED, verifier-enforced),
> `aud` binding (a stolen token only works against one target), `cti`
> uniqueness with a pinned replay cache (below), and TLS everywhere
> (§7.1 core). The rejected alternative — a detached `COSE_Sign1` over
> `(method, BLAKE3(body), timestamp)` in a second header — is sound but
> forces body-hash plumbing into every client middleware stack and a
> canonicalization story for streaming uploads before any cell of the matrix
> is green. The header name **`X-Arkavo-PoP` is reserved** for v2; v1
> implementations MUST NOT emit it and MUST ignore it.

**Replay cache (pinned, not optional):** verifiers MUST keep a `cti` replay
cache with the following properties:

- **Scope:** per verifying agent process (no shared/distributed cache is
  required or assumed in v1).
- **Eviction:** an entry MUST live at least until its token's `exp`
  (i.e. ≥ the token TTL); evicting earlier re-opens the replay window.
- **Restart behavior:** a restarted verifier has an empty cache, so tokens
  issued before the restart are replayable within their remaining TTL.
  v1's default posture is to **accept and document this window** — it is
  bounded by TLS, `aud` binding, and the ≤ 300 s TTL. Deployments MAY run
  in strict mode and reject tokens whose `iat` predates process start
  (closing the window at the cost of rejecting all in-flight tokens for up
  to 300 s after every deploy); strict mode is a verifier-local choice and
  MUST NOT be assumed of peers.

## 6. Card signing (DID-bound)

Agent Cards are signed exactly per core §8.4 (JWS, RFC 8785
canonicalization, `signatures` field excluded) with one profile restriction:

- `kid` MUST be a **DID URL with a fragment** naming the verification method,
  e.g. `did:web:agent.example.com#key-1` or `did:key:zDnae...#zDnae...`.
- Key resolution: `did:key` per the did:key method (the key is the
  identifier); `did:web` via `https://{host}/.well-known/did.json`
  (HTTPS REQUIRED). The resolved verification method's public key verifies
  the JWS. v1 resolvers MUST support `did:key` and `did:web`; other methods
  MAY be rejected as unresolvable.
- `jku` MUST NOT be used when `kid` is a DID URL (the DID document is the
  sole key source; a parallel JWKS pointer is a key-confusion vector).
- `alg` is `ES256` (same rationale as §3).
- The signing DID MUST equal `params.did` and the `sub` of CWTs the agent
  presents — one DID binds card, credential, and (v2) PoP key.

**Verification failure behavior:**

> **DECISION (reviewed 2026-06-10, split per review):** the two failure
> modes are not the same severity and get different defaults:
>
> - **Signature absent** → **degrade-with-warning**: the card is usable but
>   MUST NOT be reported as identity-verified, and the client SHOULD surface
>   that the identity extension expected a signature it didn't find.
>   (Vanilla cards are unsigned by nature; absence is a missing capability.)
> - **Signature present but invalid** (bad signature, content mismatch) →
>   **fail closed**: the client MUST refuse to use the card and MUST surface
>   the verification error. A present-but-invalid signature is evidence of
>   tampering or a broken key-rotation process, not a missing capability —
>   treating it as a warning would undercut the extension's reason to exist.
>   Key-rotation lag is mitigated by `did:web` re-resolution (the DID
>   document is fetched at verification time), not by tolerating bad
>   signatures.
> - **Unresolvable DID** (network failure, unsupported method) is neither:
>   verification is *indeterminate*; treat as absent (degrade-with-warning)
>   unless the consumer requires identity, in which case reject.
>
> When the consumer requires identity (client policy, or reliance on
> `params.did` for `aud`/authorization), every non-verified outcome is a
> hard reject. The conformance scenario `card-signature-tampered` expects
> **refusal**, not a warning.

## 7. Token acquisition (out of scope)

Agents obtain CWTs from the AIA via RFC 8693 token exchange against
`identity.arkavo.net`, exchanging a deployment credential for a short-lived,
audience-bound CWT per target. The AIA (authnz-rs) is a third party to this
contract: this specification constrains only the *presented artifact* (§§2–4)
and is satisfied by any issuance path that produces a conforming CWT. No AIA
endpoint, grant type, or client-registration behavior is specified here.

## 8. Errors

Authentication failures are **binding-level**, never JSON-RPC-level: core
§3.3.2/§7.4 place credential rejection at the protocol binding
(HTTP 401/403), and a request that fails authentication never reaches the
JSON-RPC dispatcher. The −32xxx space is wrong for this; −32008 in
particular concerns extension *declaration*, not credential validity.

| condition | HTTP | response detail |
|---|---|---|
| credential missing (and server requires identity) | 401 | `WWW-Authenticate: Bearer realm="a2a", error="invalid_request"` |
| credential malformed / signature invalid / expired / `iss` untrusted / wrong `aud` | 401 | `WWW-Authenticate: Bearer realm="a2a", error="invalid_token", error_description="..."` |
| credential valid, agent not authorized for the operation | 403 | body MAY carry detail; MUST NOT leak resource existence (core §3.3.2) |

WS-upgrade equivalent: the server fails the handshake with the same status
code (HTTP 401/403 response instead of `101 Switching Protocols`),
`WWW-Authenticate` included on 401. A connection MUST NOT be upgraded first
and then closed for an upgrade-time credential failure.

## 9. Degradation

With `required: false`, a vanilla peer that presents no CWT is served
anonymously; the extension adds identity, never removes function. With
`required: true`, vanilla peers receive 401 at the HTTP layer — the
degradation contract is explicitly void, and the card says so. Vanilla
*consumers* of a DID-signed card see a standard §8.4 JWS they may verify (if
they can fetch the key from a DID doc) or ignore; nothing in the card format
deviates from core.

## 10. Conformance

`scenarios/arkavo/identity/`:

- `cwt-auth-success` — valid CWT (all §2 claims, ES256, in-window) ⇒ request
  authenticated, operation proceeds.
- `cwt-expired` — `exp` in the past beyond skew ⇒ HTTP 401,
  `error="invalid_token"`; JSON-RPC layer never reached (wire capture MUST
  show no JSON-RPC response body envelope).
- `cwt-wrong-audience` — `aud` names a different DID ⇒ HTTP 401,
  `error="invalid_token"`.
- `card-signature-valid` — DID-bound `kid`, resolvable `did:key` ⇒ verifier
  reports identity-verified card.
- `card-signature-tampered` — one byte of canonical payload altered ⇒
  default mode: card usable, warning surfaced, NOT identity-verified;
  require-identity mode: card rejected.
