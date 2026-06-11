# TØR-G Gate v1

**Status:** Draft for review · **Extension URI:** `https://arkavo.social/ext/a2a/torg-gate/v1`
**Depends on:** core spec §4.6 (extension points), §5.4 (error mapping).
**This document is normative.**

Policy gating of A2A operations by a TØR-G evaluator, plus carriage of
sequence-integrity (SEQ) taint across agent hops. The policy *content*
(TØR-G circuits) is opaque to this spec — the `torg` crates are the
evaluator; this document pins only **where** evaluation happens and **how
refusals and taint look on the wire**. Two implementations agreeing on this
spec can disagree on every policy and still interoperate.

## 1. Advertisement

The extension URI appears in `AgentCard.capabilities.extensions` with
`required: false` — **always**. A gate that refuses an operation does so
with the on-wire shapes in §3, which vanilla peers can parse; demanding peer
*support* (−32008) would gain nothing, since the gate runs regardless of
whether the peer understands it.

`AgentExtension.params` schema (JSON Schema fragment):

```jsonc
{
  "type": "object",
  "properties": {
    "taint":        { "type": "boolean" },  // agent emits and propagates SEQ taint metadata (§4)
    "policyDigest": { "type": "string", "pattern": "^[0-9a-f]{64}$" } // OPTIONAL. BLAKE3 of the active policy bundle, hex
  },
  "required": ["taint"]
}
```

> **DECISION (proposed default):** `params` carries only `taint` (REQUIRED)
> and an optional `policyDigest` for fleet-drift detection. Policy *content*
> never rides the card.

## 2. Evaluation points

Two, and only two, in v1:

1. **Server pre-handler.** A `GatedDispatcher` wraps the SDK's request
   handler: every decoded request is evaluated before the wrapped handler
   runs. A denied request MUST NOT reach the handler (no side effects, no
   task mutation beyond the rejection task of §3.1).
2. **Client pre-dispatch.** The client-side gate evaluates an outbound
   request before it is sent. A denied outbound request MUST NOT reach the
   wire; the client SDK surfaces the refusal locally in the same §3 shapes,
   so application code handles local and remote refusals identically.

Evaluation MUST occur on the *decoded* request (post-codec), so the gate
sees the same view regardless of JSON/CBOR/transport. In-flight re-evaluation
(mid-stream revocation) is out of scope for v1.

## 3. Refusal shapes (pinned)

The split below is pinned by the plan, not a proposal: refusals of
**message-carrying** operations are *task semantics* (a rejection is a
business outcome, visible to vanilla peers as a normal rejected task);
refusals of **task-management** operations are *errors* (there is no task to
reject with).

### 3.1. Message-carrying operations

`SendMessage` and `SendStreamingMessage` refuse by returning a task in
`TASK_STATE_REJECTED` whose `status.message` is a human-readable refusal,
with structured detail in `Task.metadata` under the key
**`https://arkavo.social/ext/a2a/torg-gate/v1#rejection`**:

```jsonc
{
  "policyId": "torg:fleet-baseline@2026-06",  // string, evaluator-scoped policy identifier
  "ruleId":   "SEQ-007",                      // string, the matching rule
  "reason":   "derived from untrusted tool output", // human-readable, MAY be redacted by policy
  "advisory": false                            // bool: true = the gate would warn but policy ran in advisory mode
}
```

All four fields REQUIRED. For `SendStreamingMessage` the rejected task is
delivered as the stream's task event followed by normal stream termination —
a refusal is a *successful* protocol exchange. `advisory: true` is only ever
observed on *allowed* operations that carry the metadata as a warning
(an advisory policy MUST NOT produce `TASK_STATE_REJECTED`).

### 3.2. Task-management operations

`GetTask`, `CancelTask`, and push-notification-config operations refuse via
a JSON-RPC error:

| field | value |
|---|---|
| `code` | **−32099** |
| `message` | `"Refused by policy"` |
| `data` | §5.4-shaped array of `@type` objects, see below. The array MUST contain exactly one `arkavo.torg.v1.Rejection` object, and its `policyId` and `ruleId` MUST be populated — refusals are required to be debuggable |

```jsonc
"data": [
  {
    "@type": "type.arkavo.social/arkavo.torg.v1.Rejection",
    "policyId": "torg:fleet-baseline@2026-06",
    "ruleId": "TM-002",
    "reason": "caller not in task context",
    "advisory": false
  }
]
```

> **DECISION (proposed default):** code **−32099**, name
> `TorgPolicyRefusal`, taken from the top of the A2A-reserved custom band
> (§9.5 reserves −32001…−32099 for A2A-specific errors; −32001…−32009 are
> assigned, the remainder is the only sanctioned place for a custom code).
> Pinning the *top* of the band minimizes collision odds with future core
> assignments, which have grown upward from −32001. The `data` array reuses
> the core §5.4 structured-error shape (objects with `@type`) so generic
> error tooling renders it; the `@type` URL is ours, the envelope discipline
> is the spec's.

Implementations MUST NOT report policy refusals as −32603 (the
finding-7-class smell) or −32602: the request was valid; it was *refused*.

## 4. SEQ taint propagation

Sequence-integrity taint (flag namespace SEQ-001…SEQ-017) travels in
**`Message.metadata`** under the key **`arkavo.social/seq#taint`**, whose
value is an array of taint records:

```jsonc
"metadata": {
  "arkavo.social/seq#taint": [
    { "flag": "SEQ-003", "origin": "did:web:tools.example.com", "hop": 2 }
  ]
}
```

| field | type | meaning |
|---|---|---|
| `flag` | string `^SEQ-[0-9]{3}$` | the sequence-integrity condition |
| `origin` | string | agent that first raised the flag: its DID, else its canonical card URL |
| `hop` | integer ≥ 0 | forwarding distance from origin; 0 at the raising agent |

**Note on the key:** taint is deliberately keyed under the stable
`arkavo.social/seq#taint` namespace rather than this extension's versioned
URI — taint records are *shared substrate* read by multiple Arkavo
extensions and must survive a torg-gate version bump without a metadata
migration. This is a sanctioned deviation from the key-by-extension-URI
convention of core §4.6.2, and the only one in the Arkavo suite.

**Propagation rule (pinned):** an agent forwarding content *derived from*
tainted input MUST carry the full taint array forward on the derived
message, incrementing `hop` on each carried record by exactly 1. Records
MUST NOT be dropped, merged, or re-originated in transit. Whether a given
output is "derived from" a given input is the forwarding agent's judgment;
erring toward propagation is RECOMMENDED. What the gate *does* with taint
(block, allow, advisory) is policy, not spec.

## 5. Degradation

Vanilla peers neither send nor read taint metadata, and that is **not an
error**: the gate MUST treat an absent `arkavo.social/seq#taint` key as
"no taint asserted" and apply origin-trust per policy. A vanilla peer
receiving a §3.1 refusal sees an ordinary `TASK_STATE_REJECTED` task with a
status message (fully spec-legal); a vanilla peer receiving −32099 sees an
unknown-but-well-formed JSON-RPC error in the A2A custom band. Unknown
metadata keys are ignored per core §5.7. Nothing in this extension changes
behavior for peers that never trip a policy.

A taint chain that transits a vanilla intermediary is **broken** — the
intermediary will not propagate `Message.metadata` it does not understand.
This is accepted in v1 and is precisely what `params.taint` advertises:
gates MAY treat content arriving from `taint: false`/vanilla peers at a
different trust origin per policy.

## 6. Conformance

`scenarios/arkavo/policy/`:

- `gate-allow` — gated server, permissive policy: `SendMessage` flows to the
  handler unmodified; response carries no `#rejection` metadata.
- `gate-deny-rejected-state` — denying policy: `SendMessage` and
  `SendStreamingMessage` yield `TASK_STATE_REJECTED` with a well-formed
  `#rejection` object (all four fields); the scripted handler proves it was
  never invoked. `GetTask` under a denying task-management rule yields
  −32099 with the §3.2 `data` shape.
- `taint-propagation-blocked` — message carrying a `SEQ-*` taint record is
  forwarded through a taint-aware agent (hop increments, record preserved)
  into a gate whose policy blocks that flag ⇒ `TASK_STATE_REJECTED` with
  `ruleId` naming the flag. Control leg: same message into a permissive
  gate passes with taint intact.
