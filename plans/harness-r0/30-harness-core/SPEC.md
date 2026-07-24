# Spec: harness core — event log, replay, forcing, protocol

The `tidepool-harness` crate (name freed by 00-scaffold): session tree
state, durable event log, forcing gates, and the `tidepool-web` protocol
server. Four leaves; C2 (replay) is opus, rest sonnet. Leaf specs are
FINALIZED AT SPAWN against 00-scaffold's contracts.md — this file fixes
scope, the log schema draft, and the traps.

## ANTI-PATTERNS (all leaves)

- DO NOT invent private web↔harness APIs — every observatory capability is
  a documented protocol verb first (E1); the web UI is a client.
- DO NOT let any code path spend tokens or run effects on an unforced
  node — forcing events are the ONLY work-begins mechanism (C1/C2), and
  the consent-integrity metric is audited from the log (literal zero).
- DO NOT free-author teasers from program text — teasers are
  harness-generated (title + effect row + fan class) (C7).
- DO NOT render fan-out as a number when it's runtime-dependent — the
  badge is three-valued (exact / bounded / dynamic), converting to exact
  at materialization with a forcing-policy re-check.
- DO NOT log effect responses lazily/optionally — E4 replay is
  effect-response substitution; a missing response breaks restoration.

## HANDOFF FROM SEGMENT 20 (landed 9faa78a2)

`ResidentSession` (tidepool-runtime/src/session/resident.rs) exposes
`pending_continuation()`, `is_idle()`, `effect_names()`, and returns
`ResidentOutcome { hole, request, output }` — the hooks C3/C4 map to
`log::Event` (hole_published etc.). `SessionRegistry`
(tidepool-harness/src/registry.rs) is the `SessionId`-keyed `Slot` store
where the node tree binds `NodeId` → live session; it is `M`-generic, so
wrap `ResidentSession` in a richer node type if needed. Resident
continuation ids use prefix `"scont"`. Leaf ORDER within this segment:
C3 (node tree + forcing — owns the NodeId↔SessionId binding) BEFORE C4
(protocol server, which serves C3's tree); C2 (replay) AFTER segment 40
lands, because 40 deliberately changes the consume-on-entry continuation
contract that hole restoration depends on.

## Leaves

### C1 — event log (sonnet)
Append-only jsonl, one file per run, fsync'd per event. Header line pins
`{prelude_hash, extract_fingerprint, harness_version}`. Event kinds
(draft — finalize in contracts.md):
`node_created{node, parent, teaser, effect_row, fan}`,
`forced{node, actor}`, `turn_start{node, source|eval_input}`,
`effect{node, seq, req, resp}`, `hole_published{node, cont, site, type,
prompt, fork}`, `hole_answer_attempt{node, cont, source, outcome}`,
`hole_consumed{node, cont}`, `node_done{node, result_rendered}`,
`node_cancelled{node, reason}`. Serialize req/resp via the existing
`value_to_json` bridge.

### C2 — replay/restore (opus)
Cold-start reconstruction: re-run each node's logged sources with a
substituting handler stack (logged responses injected in sequence instead
of live handlers) until the log ends → machine lands at the same
suspension; holes re-published. Divergence check: each replayed effect
request compared (tag + canonical-json) against the logged one; first
mismatch → node demoted to browsable-history (no live restore), loudly.
Version-pin mismatch → demote without attempting (operator override is a
later rung). Uses segment 20's resident sessions; no repl involvement.

### C3 — forcing gates (sonnet)
Thunk nodes (no session, no tokens, no effects until forced);
`autoForce = never` hard-coded (the C5 ladder is R2); pre-force badges:
effect row (from the stack declaration), fan (three-valued), price class.
`returnControlFork` publishes a thunk child, never a running one.

### C4 — protocol server (sonnet, in tidepool-web)
axum; SSE event stream (the log, tailed live) + verbs: `force(node)`,
`answer(cont, source|value)`, `cancel(node)`, `eval_in_binding(node,
name, expr)`; snapshot endpoints paginate (no small-tree assumption).
Loopback bind only. Everything curl-able; examples in the crate README.
Also here: the ask-dispatch merge of segment 10's `asks.json` sidecar
into published holes (the `typedSite` lookup — contract in
10-extract-pass/SPEC.md step 6).

### C5 — model-turn driver (sonnet; AFTER C3 + segment 40; gap caught 2026-07-23)
The agent loop that makes holes model-answerable — the piece that turns
the harness from operator-console into agent runtime. On a forced
model-routed hole: create the child node/session (nested on the parent's
machine, segment 40 machinery), assemble the opening prompt (rendered
hole card: question + `Code` type sig + "you have ONE tool: eval; answer
by evaluating `resume <expr>`"), then loop: `ModelProvider::complete` →
extract the eval block from the reply → run it via the resident session →
feed back the rendered result or the GHC/type error verbatim (the error
IS the retry prompt — do not paraphrase it) → until `resume` consumes the
hole, or a turn/budget cap trips (cap trips → hole stays open, surfaces
as an attention item, operator can answer). Every provider call's Usage
goes to the log for meters; every eval source is logged (replay needs
it). Eval-block extraction: fenced ```haskell block, last one in the
reply, documented convention in the prompt — no bespoke tool-call
protocol (freeform transport per the PRD's spec-v0.1 core).

## VERIFY

Per leaf: unit tests + one integration test through the real server
binary. Segment-level: kill -9 mid-suspension → restart → tree
reconstructed, hole answerable (E4 acceptance); consent-integrity: a
`returnControlFork` request with no forcing event shows zero effect/turn
events for the child node.
