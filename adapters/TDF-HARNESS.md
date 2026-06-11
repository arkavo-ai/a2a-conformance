# TDF parts harness behavior contract (Phase 5)

How the `arkavo-ext-*` adapters realize `scenarios/arkavo/tdf/*` against
`tdf-parts-v1.md`. KAS is out of scope (spec §6): the harness injects the
unwrapped DEK directly, exactly as a real KAS would after rewrap. Follows the
identity-harness pattern (shared fixtures + Rust-generated manifest
cross-verified by Swift).

## Conformance crypto profile (pinned — both languages MUST match byte-for-byte)

The spec pins part *shapes*; for cross-language byte-identity the conformance
suite additionally pins the cipher construction (this is harness/profile
detail, not a change to tdf-parts-v1, which stays KAS-agnostic):

- **Cipher:** AES-256-GCM. Key = the 32-byte DEK from
  `adapters/shared-fixtures/tdf/test-dek.bin` (the "KAS-unwrapped" DEK).
- **Nonce:** 12 bytes, carried in the manifest as `nonce` (base64). For static
  fixture vectors the nonce is fixed (see manifest); live harness runs MAY use
  a fresh random nonce since both peers read it from the manifest.
- **AAD (additional authenticated data):** the manifest object with its
  `ciphertextLength` field present but **`nonce` and any signature excluded**,
  serialized as JCS (sorted keys, no whitespace) UTF-8 bytes. Pinning AAD makes
  the GCM tag reproducible across implementations and binds the ciphertext to
  the manifest.
- **Inline NanoTDF manifest** (the `manifest` object of shape (a)), pinned fields:
  `{"scheme":"nanotdf","v":1,"kasUrl":"https://kas.arkavo.net","nonce":"<b64>","cipher":"AES-256-GCM"}`.
  Real NanoTDF ECDH/ephemeral-key/policy-binding framing is **not** required for
  conformance — KAS is out of scope; the DEK is provided. Implementations MAY
  carry extra manifest fields; AAD excludes everything not listed above plus the
  excluded fields, so extras do not affect the tag. (Keep it minimal in v1.)
- **Integrity (shape b):** BLAKE3 of the ciphertext blob, lowercase hex, in
  `Part.url` `/b3/<hex>` and `#enc.b3`.

`adapters/shared-fixtures/tdf/manifest.json` (Rust-generated, Swift-verified):
for each plaintext vector, the inline NanoTDF `data`-part value (manifest +
ciphertext) and a shape-(b) record (blob bytes' b3 hex + the url). Swift decrypts
every Rust vector to the exact plaintext and re-encrypts to byte-identical
ciphertext; the reverse vectors (`swift-tdf-vectors.json`) are verified by Rust.

## Scenario-keyed harness behavior

Server harness (the encrypting/serving side), on `/select`:

| scenario | server behavior |
|---|---|
| `nanotdf-part-roundtrip` | the scripted handler returns a message/task whose artifact contains a shape-(a) NanoTDF `data` part (encrypt a known plaintext with the test DEK), `mediaType` + `#enc` exactly per §2, alongside one plaintext part. |
| `tdf-part-wrong-key-fails-closed` | same encrypted part, but the **client**'s injected DEK is `wrong-dek.bin` (scripted KAS denial) → client decrypt fails closed. The served bytes are identical to the success case; only the client's key differs. |
| `b3-url-artifact-integrity` | the harness writes the ciphertext blob to a local `/b3/<hex>` HTTP route **before** serving the referencing message (payload-first ordering, wire-capture-checkable), serves a shape-(b) `url` part. A `?mutate=1` variant serves a blob whose bytes don't match the advertised b3 (integrity-failed leg). |

Client harness (the decrypting side):

| scenario | client behavior |
|---|---|
| `nanotdf-part-roundtrip` | receive, locate the `#enc` part, decrypt with the test DEK, assert plaintext byte-identical to the known value; the sibling plaintext part decodes normally. Outcome `result`. |
| `tdf-part-wrong-key-fails-closed` | decrypt with `wrong-dek.bin` → GCM auth fails → the part is surfaced with `#enc` intact + `#error.code="kas-denied"` (harness maps unwrap denial to that code), **no plaintext anywhere** in the decoded view; sibling plaintext part still delivered. The A2A op itself returns `result` (exchange succeeds — fail-closed is part-scoped). The harness asserts: error metadata present, plaintext absent. |
| `b3-url-artifact-integrity` | read the `url` part, verify `/b3/<hex>` shape, fetch the blob, verify BLAKE3==`#enc.b3`: match → decrypt/accept (result); the mutate leg → `#error.code="integrity-failed"`, fail-closed. The harness also asserts payload-first ordering from the server's write log. |

`#error` is local-only (spec §4): the harness attaches it to the decoded view it
emits as the outcome value; it MUST NOT be sent on the wire.

## Degradation

A vanilla client receiving an encrypted part sees an opaque-but-well-formed
`data`/`url` part with unknown metadata (ignored per core §5.7) and reads
sibling plaintext parts normally — covered by the existing degradation row
(no encrypted-part scenario is added there; the existing core scenarios already
prove vanilla peers are unaffected by extension metadata).
