# Policy harness behavior contract (Phase 2)

How the `arkavo-ext-*` adapters realize `scenarios/arkavo/policy/*` against
`specs/arkavo/torg-gate-v1.md`. Supplements CONTRACT.md and follows the
IDENTITY-HARNESS.md pattern: scenario-keyed behavior lives in the harness;
SDK/extension code paths never see scenario ids.

## Evaluator seam (resolves how conformance exercises an opaque policy)

`torg-gate-v1.md` deliberately treats policy *content* as opaque (the TØR-G
circuits are the production evaluator). What conformance proves is the **wire
contract**: where the gate evaluates, how refusals look, how taint is carried.
Both extension layers therefore expose a `PolicyEvaluator` seam
(trait/protocol): `evaluate(op, message_or_request, taint) -> Decision
{allow | deny {policy_id, rule_id, reason, advisory}}`.

The harnesses plug in the **reference evaluator** (deterministic, shared
semantics both languages MUST implement identically):

1. deny with `{policyId: "conformance:reference@v1", ruleId: "TXT-001",
   reason: "matched deny phrase"}` when any text part contains the substring
   `policy-violating`;
2. deny with `{policyId: "conformance:reference@v1", ruleId: "SEQ-004",
   reason: "tainted input blocked"}` when the message's
   `arkavo.social/seq#taint` array contains an entry with `flag == "SEQ-004"`;
3. allow otherwise.

Production TØR-G evaluators implement the same seam; plan §7.2 (pure-Swift
mask interpreter vs FFI) is thereby narrowed to "which production evaluator
plugs into an already-pinned seam" and is NOT exercised by these scenarios.

## Scenario-keyed harness behavior

Server harness, on `/select`:

| scenario | server arming |
|---|---|
| `arkavo/policy/*` | **gate armed**: the GatedDispatcher (rs) / gated handler wrapper (swift) evaluates the reference policy BEFORE the scripted handler. Allow ⇒ scripted response flows normally. Deny on a message op ⇒ the GATE (not the script) produces the `TASK_STATE_REJECTED` task carrying `…torg-gate/v1#rejection` metadata per spec §5. Deny on a task-management op ⇒ JSON-RPC error −32099 with the `arkavo.torg.v1.Rejection` `@type` object (`policyId`/`ruleId` populated — MUST, per review). |
| everything else | gate not armed (degradation posture). |

Client harness:

| scenario | client behavior |
|---|---|
| `arkavo/policy/taint-propagation-blocked` | send the scenario's params verbatim — the taint metadata is already in the scenario fixture; the client MUST carry it untouched (spec §4 MUST-propagate). `expectRequest` verifies carriage server-side. |
| other `arkavo/policy/*` | vanilla send; assertions ride the scenario expectations (REJECTED state + metadata presence for deny; COMPLETED for allow). |

Client-side pre-dispatch gating (`ClientGate`) is part of the extension
layers and unit-tested there; no Phase 2 scenario exercises it over the wire
(a client that refuses to send produces no wire traffic to certify — noted
for a possible future scenario class).
