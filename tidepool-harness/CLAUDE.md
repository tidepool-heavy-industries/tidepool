# tidepool-harness — typed-yield session harness

A frontend over the eval substrate, peer to `tidepool-repl`. Both share ONE
suspension engine: the threadless stow-as-data mechanism in
`tidepool_runtime::session::PersistentSession`. Suspension here is threadless
throughout: no JIT continuation is ever held by a parked thread. (The
operator gate parks a thread, but it holds no continuation — see below.)

Module map:
- `tree`/`forcing` — `NodeId`/`NodeState`/`HoleId`/`SiteId`, forcing badges,
  and `NodeTree<M>`: parent/child structure + per-node lifecycle state,
  backed by the durable event log. Generic over a machine handle `M` and
  backed internally by a `SessionRegistry<M>` (see Machine lifecycle below).
- `registry` — `SessionRegistry<M>`: the `Idle | Running{holes} |
  Suspended{machine, holes}` slot machine (MULTI-HOLE: a suspended session
  carries a SET of parked holes, each resumable by identity in any order) +
  atomic checkout/restore, including a child-run checkout over parked frames
  (`checkout_child`).
- `harness` — `Harness`: the orchestrator. Owns a `NodeTree<Session>` whose
  `SessionRegistry<Session>` is the one place a resident session lives (see
  Machine lifecycle below); a separate `convos` map holds everything ELSE
  per-node (transcript, pending hole, framing, turn lease). Drives the turn
  loop, hole classification, fork/fanout registration, elaborator proposal
  confirm/reject.
- `engine` — the turn engine: prompt assembly, provider call, extract+compile
  the last fenced Haskell block, classify a suspension (`AskWith`/
  `AskUserWith`/`RunLLMTurnWith`/`FinalizeWith`) by its request's constructor
  name. `compile_turn`/`compile_turns` (`CompiledTurn`: `CoreExpr` +
  `DataConTable` + `asks.json` sidecar) are thin wrappers over
  `tidepool_runtime::artifacts::compile_targets` — the actual
  spawn/read/deserialize/diagnostics/memo mechanics moved to that crate
  (architecture review finding 3, 2026-08-17: this crate no longer owns a
  second compiler frontend). See Compile memo below.
- `log` — event-log wire schema (header pins prelude+extract fingerprints).
  `Event::TurnStart{source}` carries the EXTRACTED executed Haskell block, not
  a "model" tag; `Event::Effect{req,resp}` is written by the live
  turn loop (`Harness::flush_effects`, drained per turn in `run_block`/`answer_*`)
  whenever a turn dispatches a HANDLED effect — see Replay below for the
  substitution boundary and the scoped-stack caveat.
- `provider` — `ModelProvider` trait (calling-model turns; not the Llm
  effect) + `provider/{api_key,http,oauth,paths}` impls.
- `replay` — `ReplayProvider` (turn substitution) + `fold_tree_state`
  (crash-replay tree reconstruction) — see Replay below.
- `snapshot` — frozen post-coalgebra context prefixes: `SnapshotDigest`
  (blake3 over the exact prefix `engine::assemble_request` re-emits) and the
  immutable `ContextSnapshot` the harness interns — see Context snapshots below.
- `synopsis` — `type_document`, the full GHC-style `data` declaration a hole
  card's shape line reads, derived from a compiled `DataConTable` (which
  carries field TYPES as well as NAMES).

## Compile memo — one content-addressed cache, no cache-free path

Every turn compile (`engine::compile_turn`/`compile_turns`, wrapping
`tidepool_runtime::artifacts::compile_targets`) is memoized. There is no
cache-free path, no bypass flag, and no second mechanism. The full keying
spec, the correctness hazards it answers, and the safety argument for
sharing one memo across test processes are in `plans/compile-memo.md`; what
a reader here needs:

- **The mechanism is `tidepool_runtime::cache`, not a fork of it.**
  `invocation_key` keys the COMPLETE invocation — source CONTENT, the built
  `ExtractCmd::argv()` walked against an ALLOWLIST, the include roots by
  CONTENT with paths RELATIVE to each root, and the extract binary by content.
  An argv element the allowlist does not classify makes the invocation
  UNCACHEABLE (compile cold), never silently unkeyed — that is also how
  session-scope compiles (`--session-bind`/`--inject-val`/`--session-root`,
  which read per-session MUTABLE dirs) stay out of v1. The session lane
  (`tidepool_runtime::session::turn`) is untouched.
- **A hit stores and restores the FULL artifact set** this module reads —
  `meta.cbor`, every `<target>.cbor`, and the asks sidecar in whichever shape
  the target count selects, with ABSENT distinct from empty. Hit and miss
  rejoin at `tidepool_runtime::artifacts::assemble`, so observational
  identity is a property of the code shape. The one deliberate difference: a
  hit records no `extract_spawn` timing stage and no `extract.*` phases,
  because nothing was spawned.
- **Tests share the memo, not their state.** `tests/support::isolate_cache`
  still isolates `XDG_CACHE_HOME` per test (checkpoints, transcripts,
  `log.jsonl`, KV, the generated effects module) but points
  `TIDEPOOL_COMPILE_CACHE_DIR` at the AMBIENT cache dir so every test process
  shares one memo. Sharing is safe by construction: content-addressed entries
  are only reached by identical compilations. Measured on
  `golden_path + acceptance_askuser + selfharness_spine`: 119s before, 99s
  cold, **47s warm**.
- **A test that MEASURES compile cost must opt out** via
  `support::isolate_compile_memo()` (a fresh memo dir), or its receipt becomes
  a receipt about cache state. `acceptance_boot_compile_count` is the one such
  test — its `PRE_MODEL_EXTRACT_COMPILES` counts spawns, which a warm memo
  drives to 0.

## Machine lifecycle — the registry is the one session-lifecycle truth

`Harness` instantiates its `tree` field as `NodeTree<Session>` (`Session =
ResidentSession<BoxedStack, CapturedOutput>`), so `NodeTree::force`'s
caller-supplied machine IS the real resident session — the tree's internal
`SessionRegistry<Session>` (`registry.rs`) is the ONLY place a session lives.
There is no second, hand-rolled take/put discipline: a turn-owning method
checks a node's machine OUT via `Harness::checkout_run`/`checkout_resume`/
`checkout_child` (thin wrappers over `SessionRegistry::checkout_run`/
`checkout_resume`/`checkout_child` that resolve the node's `SessionId` via
`NodeTree::session_of` and map a refusal through `HarnessError::from_checkout`
— the one place a `CheckoutError` becomes a node-scoped error), runs the turn
on the blocking pool via `Harness::run_checked_out`, and restores it with the
session's OWN post-call reported hole SET (`Session::parked_holes()`) through
one unified `Checkout::restore_suspended` call — an empty set IS `Idle`, so
there is no separate idle/suspended restore to desync — not a guess from the
turn's domain result, so an errored `run`/`resume` still restores correctly.
`run_checked_out` also applies the node's realm to the machine before the turn
(`Session::set_realm`) at this one site, so an ATTACHED answerer node's parks
are always owned by its own realm on a shared machine (see Self-iterating
harness below).

MULTI-HOLE (one-session plan, Phase 2): a suspended session carries a SET of
parked holes, each resumable by identity in any order (the machine's
continuation registry imposes none) — a NEW top-level run over parked frames
is an ordinary `checkout_run`, not a refusal; the old reject-while-suspended
behavior and the separate `RunningChild` slot variant are both gone
(`Slot::Running{holes}` covers a fresh run, a resume, and a child run over
parked frames alike). A `checkout_child` (a CHILD run over a suspended
session's parked frames — the discipline an answer value crosses by: a
non-consuming child run against the TARGET's own session) requires at least
one parked hole and is otherwise an ordinary checkout: with the continuation
registry there is no special child window, and the parked holes ride the
checkout like any other turn's.

`Checkout` is panic-safe: if a checkout is dropped without an explicit
restore (a panic unwinding between checkout and restore, before the machine
was ever moved off the checkout via `take()`), `Drop` restores it with the
hole SET it carried out — `Suspended{holes}`, or `Idle` only when that set is
empty — rather than leaving the registry slot wedged `Running` forever, and
without losing frames that are still rooted in the machine. The one case
`Drop` cannot cover is a machine already moved onto the blocking pool via
`take()`: if that task panics (`JoinError`), the machine is genuinely gone —
`run_checked_out` calls `Harness::terminate_node` instead of trying to
restore a machine it does not have.

`Harness::terminate_node` is the ONE retirement path: idempotently
terminalize the tree entry (`NodeTree::node_cancelled`, skipped if already
`Done`/`Cancelled`), then retire the SESSION according to who owns it. An
OWNING node (the ordinary case) has its session removed from the registry
(`SessionRegistry::remove`, dropping the machine); an ATTACHED node (the
one-session collapse's per-loop answerer — see Self-iterating harness below)
never owns the shared session, so its retirement is realm SCOPE EXIT
(`close_realm` on the shared machine: the realm's parked frames and any
outstanding `ValueHandle`s are released together, sibling realms untouched) —
the outer session outlives every answerer node it hosts. Either way the
node's `convos` entry is removed. `cancel`, a failed fork/fanout child's
cleanup, the `JoinError` path above, and the self-iterating harness's
`retire_answerer` all retire a node through it — there is no second way to
retire one. A busy node (`CheckoutError::Running`) surfaces as
`HarnessError::TurnInFlight`, never `NoSession` — that variant is reserved
for a node that genuinely has no session (never forced, or already
terminated).

`convos: Mutex<HashMap<NodeId, NodeConvo>>` still holds everything a session
checkout doesn't: the transcript, the pending hole, per-node framing, the
answer contract, the turn lease. A read that needs the session's own state
WITHOUT checking it out (decl-plane context for a session-aware compile, the
session's import module) goes through `SessionRegistry::peek`, which succeeds
only when the machine is actually present in its slot (`Idle`/`Suspended` —
not `Running`/`RunningChild`, checked out elsewhere).

## Replay — turn substitution + crash-replay tree reconstruction, NOT effect replay

Two independent pieces, both in `replay.rs`:

- **Turn substitution** (`ReplayProvider`): a `ModelProvider` that serves
  previously-recorded assistant `TurnDelta` replies back in order instead of
  calling a live model — a CI run re-drives the same golden path with zero
  API calls.
- **Offline log inspection** (`fold_tree_state`/`FoldedTree`): folds a log's
  events into the terminal per-node `NodeState` + tree structure, so a
  finished or crashed run's browsable history tree can be reconstructed from
  the durable log alone — an inspection tool, in the same family as
  `tail -f log.jsonl`. It is NOT the startup recovery path: that is the
  generation-tagged `persistence::Checkpoint` the driver restores from at
  boot (`SelfHarnessDriver::restore`) — a second recovery source folding the
  log at startup would be dual lifecycle machinery. `golden_path`'s
  crash-replay assertion (a killed process's log folds back to the terminal
  tree) is what pins this contract.

**Effects are RECORDED live; they are never SUBSTITUTED on replay.**
`Harness::flush_effects` drains a node's `effect_trace` after each
`run_block`/`answer_*` into `NodeTree::effect`, so every turn that dispatches
a HANDLED (non-suspending) effect writes one `Event::Effect{req,resp}` per
effect. A SUSPENDING effect (`Ask`/`AskUser`/`RunLLMTurn`/`Finalize`) never
reaches a handler, so it logs as `HolePublished`/`HoleConsumed`, not `Effect`.

**Scoped-stack caveat:** the self-iterating harness's answerer (`[AskUser,
Finalize]`) and outer loop
(`[RunLLMTurn, AskUser]`) declare ONLY suspending effects — no base
`Console`/`Fs`/`Http`/… — so `flush_effects` runs but drains an empty trace:
those nodes produce NO `Event::Effect` BY CONSTRUCTION (that absence IS the
capability boundary — the answerer structurally cannot run a shell/file/net
effect). A general Agent node (full base-effect row) does produce them.

**The reserved gap:** nothing READS those records back. Recorded responses are
never substituted into a resumed session, so a node that suspended after
running handled effects, then restarted and resumed, RE-EXECUTES them live.

## Scope trees — a window's names retire with its heap (PRD 21 C2 §1–3)

A node already carried a `RealmId`: the HEAP-side lifetime of its window
(parked frames, outstanding `ValueHandle`s), exited by `close_realm`. C2 gives
it the NAME-side one alongside — a `ScopeId` (`tidepool_codegen::scope`), the
frame both session planes hang their per-window declarations and bindings off.
One window, two halves, **one** retirement step.

**How a window gets a scope.** Mint it off the session
(`with_session(sid, |s| s.mint_scope(parent))` — `PersistentSession` owns the
one `ScopeTree`, and minting is also where the DECL plane seeds the child's tip
from its PARENT's, so a sibling that defines in between cannot leak in), then
`Harness::set_node_scope(node, scope)`. `Harness::run_checked_out` applies it
to the session (`ResidentSession::set_scope`) at the SAME one site it applies
the realm, so every run/resume/child path is covered and a node WITHOUT a scope
runs at `ScopeId::ROOT` — never at whatever scope the last turn on a shared
machine left behind (the ambient-stickiness hazard the realm reset already
answers). `with_session`'s own runs reset to ROOT for the same reason.

**What a scoped turn actually compiles against.** `session_bind_context` and
`session_decl_context` resolve the turn's decl-tip import module and its
visible `Val.G<g>` set FROM THE NODE'S SCOPE (`session_import_module_in`,
`current_val_modules_in`), and a decl turn appends to that scope's own tip
(`define_scoped_in`). That is the whole of locked decision 4 on the real
compile path: a child's tip module already re-exports its parent's chain, so
parent declarations are callable in every child; the visible-binding walk is
upward-only with child frames shadowing parent ones, so a sibling's names are
not even *nameable*; and nothing ever walks downward, so the parent gains
neither. A value bind lands in the turn's own scope, and the cross-plane rule
(a name lives in at most one plane) is scoped with it — `materialize_binder`
retracts the decl head in the BINDING's scope, so a child binding `helper`
never retracts the parent's. At ROOT every one of these is the pre-C2 path
verbatim; `tests/companion_mount_spike.rs` passing unmodified is the gate.

**What retirement releases.** `Harness::terminate_node` is still the ONE
retirement path. For an ATTACHED node it now exits both halves in
`exit_window`: `close_realm(realm)` first (parked frames + handles), then
`retire_scope(scope)`. That order is load-bearing — scope retirement's
sole-ownership rule reads the handle registry, so a handle the realm still
owned would wrongly pin a root. The immediate path and the QUEUED path
(`pending_window_exits`, drained by whichever code path next holds the machine)
go through that one function, so they cannot diverge; both halves of the
window's identity are retained in the queue until the exit is CONFIRMED. An
OWNING node is unaffected: its whole session is dropped.

The receipt outlives the node. `retire_scope` returns
`ScopeRetirement { scopes_retired, bindings_retired, roots_released }`, and
`terminate_node` records it under the node id for `Harness::scope_retirement`
— by the time a caller checks the ledger the `convos` entry is gone, and
`roots_released` is the only thing the ledger's movement can be checked
against.

**The four counted classes, and their harness-visible reads.** Never folded
together — the full table and the reasoning live in `tidepool-codegen/CLAUDE.md`
§ root accounting; what a caller here needs is which read to take:

| # | Class | Read (through `with_session`) | Scope retirement |
|---|-------|-------------------------------|------------------|
| 1 | parked continuations | `s.stowed_roots_count() == s.parked_count()` | UNCHANGED |
| 2 | handle registry | `s.value_handle_count()` | UNCHANGED (a mount already transferred out) |
| 3 | value-plane bindings | `s.binding_names()` (ROOT) / `s.scope_binding_count(scope)` | the retired scope's frame goes to 0 |
| 4 | GC root ledger | `s.persistent_roots_count()` | drops by EXACTLY the receipt's `roots_released` |

Classes 1 and 2 staying put is an assertion, not an expectation: a parked
frame's root belongs to a REALM, and a mounted root left the handle registry at
the mount. Class 4 is the witness — without it class 3 can return to baseline
while every root stays traced.

**Deregistered is not reclaimed.** Retirement removes a root from the GC TRACE
LIST; it does not free `OldSpace` bytes (no major or compacting pass exists).
A long-resident session's OldSpace grows monotonically with the total number of
mounts ever made and is reclaimed at machine drop or rotation. Written here,
in `tidepool-codegen/CLAUDE.md`, and in the design doc so nobody re-derives it
while hunting a leak.

**Escaped closures stay alive by REACHABILITY, not by exemption.** A closure
finalized in a child scope is mounted into a PARENT-scope binding
(`mount_handle_in(parent, …)`, the C1 mount seam pointed across a scope
boundary). That parent binding owns its own `RootSlot`, so retiring the child
leaves it registered while the child's own bindings' roots go, and the captured
child-heap objects stay traced transitively through it. Gate:
`tests/companion_scope_trees.rs`.

## Context snapshots — one frozen prefix, many branches (PRD 21 C2 §4)

The boundary already existed and was never named: `register_fork_child`
computes `checkpoint = parent_transcript.len()` and seeds the child with the
parent's transcript AND framing, and `engine::assemble_request` is
`[system(framing ?? SYSTEM_FRAMING)] ++ transcript` verbatim — so a fork
child's assembled request prefix has always been byte-identical to its
parent's through the checkpoint. `snapshot.rs` gives that prefix an identity
and receipts; it does not invent it.

- **`Harness::freeze_snapshot(node) -> SnapshotDigest`** — the explicit
  operation. Interns an immutable `Arc<ContextSnapshot>`. IDEMPOTENT: an
  unchanged transcript freezes to the same digest, does not duplicate the
  entry, and writes no second `SnapshotFrozen` receipt. Nothing is ever
  evicted, so a digest a child was minted from always resolves.
- **The digest runs over the ASSEMBLED prefix**, via `assemble_request`
  itself (`snapshot::digest_prefix` calls it) — one assembly path, so the
  digest cannot drift from what a provider is actually sent. Domain-separated
  (`b"tidepool-context-snapshot-v2"`) and length-framed, mirroring
  `tidepool_runtime::cache`'s idiom; `frame` is private there, so the same
  three lines are reimplemented rather than a second scheme invented. Covers
  each message's `reasoning_items` too (bumped to v2, 2026-08-20): the
  provider request genuinely echoes them onto the wire
  (`provider::oauth::to_input_items`), and `Message` being `Clone` means a
  fork child DOES carry them — so two transcripts identical in role+content
  but differing in reasoning state are different provider-visible prefixes,
  and must not intern to the same `SnapshotDigest`. Still in-memory only:
  digests are never written into `persistence.rs`'s checkpoint, so the v2
  bump needed no restart-compat handling.
- **`Harness::fork_from_snapshot(digest, brief)`** goes through the same
  `seed_forked_child` an ordinary fork does — the ONE place a
  `NodeSeed::Forked` is staged — so forcing, seeding, framing inheritance, and
  the `TurnForked` checkpoint cannot diverge between the two. Every sibling
  reports one parent digest (`Harness::branch_snapshot`).
- **Immutability is locked decision 2, and it is pinned.** A `ContextSnapshot`
  is never mutated; an interned snapshot and each child own their own copies,
  so there is no `&mut` path to a frozen prefix. Compaction
  (`replace_transcript_with_summary`, destructive to the node's LIVE
  transcript by design) therefore cannot reach one: a node that HAS a frozen
  snapshot gets a NEW one minted at compaction — a new digest, a new cache
  root, a second receipt — while the old entry and every existing child stay
  exactly as they were. `tests/companion_snapshots.rs` asserts this by
  re-digesting the children's own assembled prefixes AFTER the parent is
  compacted.

### The provider cache-metric gap (read this before claiming a cache win)

**No provider impl in this tree emits `cache_control` breakpoints, and until
C2 none parsed a cache metric. Measured cache REUSE is therefore not
verifiable from our side.** Stated plainly so C6's dogfood does not go looking
for a number that isn't there:

- `TurnRequest` has no metadata slot at all, and nothing anywhere emits
  `cache_control`. We do not *cause* provider-side cache hits; at most we make
  a stable prefix available for a provider to cache on its own terms.
- `Usage` now carries `cached_input_tokens: Option<u64>`, populated ONLY from
  a field the response genuinely has — the Responses-API SSE usage object's
  `input_tokens_details.cached_tokens` (`provider/oauth.rs`) and genai's
  `usage.prompt_tokens_details.cached_tokens` (`provider/http.rs`). A provider
  that reports nothing leaves it `None`. **`None` means NOT REPORTED and is
  serialized as an absent field — never `0`.** Nothing synthesizes it, and
  prefix identity is never treated as evidence of a cache hit. `Usage` also
  carries `cache_write_tokens: Option<u64>` on the same discipline, from
  `input_tokens_details.cache_write_tokens` — the OAuth `/responses` call now
  also sends the body field `prompt_cache_key` (equal to the `session-id`
  header value), matching the official Codex client's cache-routing contract;
  `session_id_for`'s derivation is unchanged, only what carries its value.
- There is no local tokenizer here, so a token-level split of a shared prefix
  is not claimed. What IS verifiable, and what the receipts carry:
  - the **digest** — the frozen prefix's identity, recomputable by anyone;
  - the **byte counts** — `shared_prefix_bytes` / `branch_suffix_bytes`, exact
    UTF-8 content bytes of the assembled prefix and of what a branch added;
  - the provider's **own `input_tokens`** for the branch's first turn.
- Two prefix-stability hazards a future cache-breakpoint lane inherits: the
  self-iterating outer loop recomposes its SYSTEM message per iteration
  (iteration count, operator input, rotation losses), so message 0 is not
  stable across loops; and compaction rewrites the framing carried into the
  next render. Neither blocks digest identity — the digest covers framing
  explicitly — but both cap what a cache claim could cover.

Receipts: `Event::SnapshotFrozen{node,digest,messages,prefix_bytes}` at the
freeze, `Event::BranchInvocation{node,snapshot,shared_prefix_bytes,
branch_suffix_bytes,input_tokens,cached_input_tokens}` at a snapshot-forked
branch's FIRST turn (one-shot). The `Usage` widening is additive +
`serde(default)` — the precedent is `Event::TurnDelta`'s `reasoning` — so
`log.jsonl` files written before it still deserialize.

**Per-round usage lives on `Event::TurnDelta.usage: Option<Usage>` (nested,
NOT a top-level key) — every assistant turn, not just a branch's first one.**
`BranchInvocation` above is a one-shot receipt scoped to a snapshot-forked
branch's opening turn; `TurnDelta.usage` is written by every model round
(`Harness::drive_turn`/`summarize_turn`/`drive_answerer_to_value`), including
corrective-retry and answerer-loop rounds, so within-window round-to-round
cache ratios are already computable from any `log-*.jsonl`:
`jq -c 'select(.event.ev=="turn_delta" and .event.usage != null) |
{node: .event.node, turn: .event.turn, ratio: ((.event.usage.cached_input_tokens
// 0) / .event.usage.input_tokens)}' log-*.jsonl`. Look under `.event.usage`,
not a flat `.event.cached_input_tokens` — that top-level shape only exists on
`BranchInvocation` lines.

#### Within-window round-to-round cache affinity (fixed 2026-08-20)

Run-4's dogfood measurement showed only 45% of input tokens served from
OpenAI's prompt cache, with consecutive model rounds inside ONE window going
0% / 59% / 0% / 68% / 0% — an append-only conversation should approach 100%
from round 2 on. Diagnosis (`engine.rs`'s `within_window_*_is_prefix_extension`
tests): **`engine::assemble_request`'s output was already a byte-identical-
prefix extension every round**, for all three within-window shapes (an
ordinary next round, a corrective-retry round with a freshly-embedded GHC
error, and a round following a note/`askUser` resume) — `convo.transcript` is
`.push()`-only everywhere (`push_user_turn`, `drive_turn`'s assistant append,
`summarize_turn`), `convo.framing` is set once at node creation and never
rewritten mid-window, and `answer_dialog`/`answer_note` resume the suspended
Haskell continuation directly without touching the transcript at all. No
content near the top of the request was being re-rendered.

The actual defect was one level down, in `provider/oauth.rs`'s
`codex_responses`: the `session-id` header sent with every `/responses` call
was a **fresh `Uuid::new_v4()` minted on every single HTTP request**, never
reused across rounds of the same window. The reference Codex CLI mints ONE
`session_id` per conversation and reuses it for every turn; the ChatGPT Codex
backend uses it to route repeat requests to the inference replica already
holding that conversation's cached prefix (the same reasoning that already
justified `x-codex-installation-id` being process-stable, just applied one
level down to the conversation). A random id every round defeated that
routing on every round — the alternating hit pattern was occasional
coincidental routing collisions, not content instability.

Fix: `session_id_for(instructions, opening)` derives a deterministic
`Uuid::new_v5` from the window's two genuinely-invariant pieces —
`instructions` (the framing) and the window's OPENING message (the first
non-system item) — both fixed for the life of a window by the prefix-
stability property above, so the header is now identical across every round
of one window while still varying across different windows. Not threaded
through `NodeId`/a session table: `ModelProvider::complete` carries no
session identity (one provider instance is shared across every node in a
harness), so deriving from content already in `TurnRequest` avoided widening
that trait. One accepted consequence: a snapshot-forked branch child, whose
opening message is inherited verbatim from its parent at the fork checkpoint,
shares the parent's `session-id` for as long as that inherited message stays
its first one — harmless (arguably a cache-affinity win, since both share the
ancestor prefix) under `store: false`, where the header is routing/telemetry
only, never persisted conversation state.

**2026-08-20 follow-up:** `session-id` alone is a Codex protocol header, not
the documented cache-routing control — that is the body parameter
`prompt_cache_key`, and the official Codex client sends both with the same
session identity. `codex_responses` now sends `prompt_cache_key` equal to
`session_id_for`'s output alongside the unchanged `session-id` header;
`session_id_for`'s own derivation is untouched. Separately, `Usage` now
captures `cache_write_tokens` (from `input_tokens_details.cache_write_tokens`
on the SSE usage object) alongside `cached_input_tokens`, so a cold-but-wrote-
cache round is distinguishable from a wasted rewrite in the durable log.

**Backend canary (2026-08-20, live probes against
`chatgpt.com/backend-api/codex/responses`):** `prompt_cache_key` is ACCEPTED
(HTTP 200; usage confirms `input_tokens_details.cache_write_tokens` on the
wire). `prompt_cache_options` is REJECTED (`Unsupported parameter`) and
`prompt_cache_breakpoint {mode: explicit}` is REJECTED (`not supported on
this model`) on BOTH `gpt-5.6-terra` and `gpt-5.6-sol` — explicit cache
breakpoints are unavailable on this backend, so fold-shaped windows (fresh
transcript + unique opening prompt, e.g. the companion's `runLLMTurnFork`
algebra windows) are STRUCTURALLY cold: 0% cached on their first round is
expected, not a defect, and no request-shape change can fix it here. Do not
re-probe without reason; do not build breakpoint support against this
backend. Platform-doc facts that govern interpretation: `prompt_cache_key`
is REQUIRED for reliable matching on gpt-5.6+; ~15 requests/minute per key
before overflow misses; 1024-token strict minimum prefix; 30-minute TTL
refreshing on reuse; cache WRITES bill at 1.25× on gpt-5.6+. Bucketed
sub-keying for high-fanout bursts is deliberately deferred until
post-`prompt_cache_key` dogfood data shows sibling scatter persisting.

`SnapshotDigest`/`digest_prefix` were UNCHANGED by THIS fix — the
`prompt_cache_key`/`session-id` header change is provider-side request
serialization, never covered by the digest, so nothing here shifted or
needed re-verifying for restart compat. (Separately, `digest_messages` was
later widened to cover `reasoning_items` — see the v2 note in Context
snapshots above; that was its own fix, for a different defect, not this
one.) Digests are not written into `persistence.rs`'s checkpoint at all —
confirmed by grep — they're interned in-memory per-run and re-minted at
freeze, per the Context snapshots section above.

**Turn boundary — recomposed SYSTEM message per loop iteration — is NOT the
same hazard and was deliberately left alone.** Each self-iterating-harness
loop iteration creates a brand-NEW per-loop answerer `NodeId`
(`SelfHarnessDriver::create_root_framed("loop answerer", …,
self.answerer_framing.clone())`) with its OWN from-scratch transcript — so
"message 0 differs across loops" is really "these are two different
conversations, not a continuation of one," and every round WITHIN each of
those per-loop windows already gets this wave's stability fix. Making the
framing byte-stable ACROSS loop iterations would mean literally reusing one
session/transcript for the whole outer loop instead of a fresh answerer node
per iteration — a real architectural change (touches how `SelfHarnessDriver`
mints per-loop nodes and threads `answerer_framing`), not a move-to-tail, and
out of scope here. Compaction's framing rewrite (the other named hazard) is
the same shape: it deliberately starts a fresh compacted prefix by design
(`replace_transcript_with_summary`), not an in-place edit of a live window.

## Invariants

Forcing events are the only work-begins mechanism (consent integrity audits
to literal zero — `NodeTree::force` is the only transition out of `Thunk`,
and it logs `Event::Forced{actor}` before any session exists); teasers are
harness-generated only (`forcing.rs::derive_teaser`).

## Self-iterating harness — the answerer row + the `AskUser` operator gate

The self-iterating harness's answerer Agent (`selfharness::driver::answerer_decls`)
compiles against `Eff '[AskUser, Fork, ReadState, Finalize]` — decl-only effects, disjoint
from the general Agent stack's `standard_decls()` (which keeps `Ask`,
`RunLLMTurn`, and every base effect untouched; `AskUser` never appears
there). `AskUser` (`tidepool_mcp::askuser_decl`) is a brand-new effect, not a
rename of `Ask`: `Ask` suspends `ask schema prompt` to the CALLING LLM AGENT
with a JSON Schema; `AskUser` suspends `askUserRaw :: Value -> M Value` (the
raw wire escape; the typed surface authors write is `askUser @T`, plus
`choose`/`chooseMany` for value-defined alternatives — `Tidepool.Form`) to a
HUMAN OPERATOR with a typed [`FormShape`]
(`selfharness::operator`) — the ONE operator-presentation algebra, carried
bare end to end — routed by CONSTRUCTOR NAME (`AskUserWith`) in
[`engine::classify_hole`] — no JSON-key probing.

`askUser @T` derives its form from `T`'s own `GHC.Generics` representation
(`Tidepool.Form.GForm`) with no value of `T`, ships it as the RECURSIVE
`FormShape` wire (`Tidepool.Form.Wire` encodes exactly the JSON
`selfharness::operator`'s module docs specify), and rebuilds the typed value
from ordinary JSON submitted by the operator. `engine::decode_askuser_spec`
decodes that one bare shape straight through for the gate and observer — no
wrapper struct. `Tidepool.Form` is
auto-imported into a turn's preamble whenever `AskUser` is in the compiling
decl list (`tidepool-mcp`'s `pragmas_and_imports`/`session_decl_module_env`);
it depends on `askUserRaw`, so it is REACHABLE ONLY on the answerer stack, not
the general eval/Agent surface.

`Tidepool.Form.note :: Text -> M ()` is a SIBLING, non-blocking display
channel riding the SAME `AskUser` GADT as a second constructor (`NoteWith`,
`noteRaw`) — `note "why I'm about to ask this"` posts text to the operator
GUI's accumulating feed and the driver resumes with `()` IMMEDIATELY, never
presenting anything via `OperatorGate::present_form`. Routed by constructor
name into [`crate::engine::HoleRouting::Note`], serviced by
`Harness::answer_note` (the audited resume path, minus the operator wait) and
`SelfHarnessDriver`'s note-draining helpers wherever an `askUser` chain can
appear (the nested answerer, the AUTHORED outer loop, and interleaved
mid-chain in either) — never counted against `ASKUSER_MAX_REPROMPTS`, since
nothing here waits on a human to spin.

`ReadState` (`tidepool_mcp::readstate_decl`, answerer row only) is the
agent-computes-over-its-own-state effect from `plans/companion-state-v2.md`:
`getStateJson :: M Value` suspends on `ReadStateWith`, routed by constructor
name into `HoleRouting::ReadState` and serviced note-style — the driver
resumes IMMEDIATELY with the loop's current state JSON
(`SelfHarnessDriver.cycle_state_json`, the same JSON the checkpoint holds; no
operator, no model round, never counted against any cap). Freshness is
trivially correct because state changes only at loop boundaries — every
window in a loop reads the state that loop started with. A driver context
with no cycle state (the general Agent path) resumes with JSON `null`.

### The answer contract — `finalize` is pinned by the ROW

An answerer turn does not compile against a polymorphic `finalize`. While a
node is answering a typed hole it carries an `AnswerContract` (set per hole by
the driver via `Harness::set_answer_contract`, since the per-loop answerer node
is reused across holes whose types differ), and its turns compile with:

- **`Finalize` instantiated at the hole's type IN THE ROW.** `Finalize` is
  type-indexed — `data Finalize v a where FinalizeWith :: Int -> v -> Finalize
  v a`, `finalize :: forall v a effs. Member (Finalize v) effs => v -> Eff effs
  a` — so a turn answering a `Decision` hole compiles against `'[AskUser, Fork,
  ReadState, Finalize Decision]` and `Member (Finalize Decision)` IS the pin. Canonical
  freer-simple, the same shape as `State s`. A wrong-typed answer is an
  ordinary GHC error naming the row (`'Finalize Text' is not a member of the
  type-level list '[AskUser, Fork, ReadState, Finalize Decision]'`), which the
  corrective-retry loop feeds back. Nothing is shimmed, hidden, or
  qualified-aliased: the turn uses the ordinary `build_preamble`.

  The tyvar shape is load-bearing and unchanged: `v` first (so `finalize @T x`
  binds it), `a` genuinely free (`finalize :: forall v a effs. Member
  (Finalize v) effs => v -> Eff effs a` never constrains `a` to anything —
  `finalize` diverges, it never returns), `Member` a real constraint (so the
  dictionary rides as the leading value arg `Translate.hs` re-applies when
  head-swapping to `finalizeSited`). Extract is untouched by the indexing —
  `asks.json` records the site type exactly as before, and `v` is erased in
  Core, so `FinalizeWith` keeps its arity and `Finalize` its positional union
  tag.

  **`a` being free makes the shared template's `toJSON _r`/`toWire _r`
  ambiguous, and GHC defaulting does NOT rescue it.** Even under
  `ExtendedDefaultRules`, with the preamble's explicit `default (Int, Double,
  Text)`, defaulting fires only when the ambiguous variable's constraint set
  carries at least one class from GHC's own standard set (numeric, `Show`,
  `Eq`, `Ord`). `ToJSON`/`ToWire` are ordinary superclass-less library
  classes, so a solitary `ToJSON a0` never qualifies: `_r <- __user; …
  (toJSON _r)` is ambiguous by construction whenever a turn's block
  terminates in `finalize`. (Not an `Eff`-row, `MonoLocalBinds`, or
  implication artifact — a plain `IO` repro fails identically.)
  `template_turn_for` (`engine.rs`) supplies the missing anchor: a turn
  compiled against a real (non-`NoAnswer`) `Finalize T` row routes through
  `tidepool_mcp::template_haskell_anchored`, which passes `_r` through a
  generated `__anchor :: P.Show a => a -> a; __anchor = P.id` before
  rendering. It is ADDITIVE — `id` never forces `_r`'s type — so an
  already-concretely-typed result (an ordinary eval, or an answerer turn that
  suspends on `askUser` without finalizing) is unaffected, and only
  `finalize`'s genuinely-ambiguous `_r` newly resolves (to `Int`: the first
  `default` candidate carrying both `Show` and `ToJSON`/`ToWire`). Every
  other caller of `tidepool_mcp::template_haskell` is untouched.

  `EngineConfig::turn_target` resolves one turn's compile target, returning a
  `TurnTarget { include, stack }` derived from a SINGLE `tidepool_mcp::RowArgs`
  — the effects-module dir (via `ensure_effects_module_at`) and the promoted
  row string (via `build_effect_stack_type_at`) come from the same place, so
  they cannot name different rows. Because the row lives in the generated
  `type M`, a pinned turn gets its own effects-module dir; the dir is
  content-addressed on the generated source, so two answer types can never be
  served each other's module and a repeat of the same type is free. The
  generated module also imports the contract's author modules — naming
  `Decision` in `type M` needs it in scope THERE, not only in the turn module.

  A turn with no contract compiles at `Finalize NoAnswer` — an uninhabited type
  declared by `Finalize` itself. Such a turn is not answering a typed hole and
  therefore has no finalize capability at all, which is the true statement, and
  GHC says it by name. Because the row admits exactly one answer type, a
  wrong-typed answer cannot compile — it can never cross in-heap into a
  `T`-typed continuation and case-trap past every check.
- **The author modules that define the type** — `HarnessSource::answerer_imports`,
  derived structurally as the sibling modules the harness file itself imports.
  Not a naming convention: a module the harness does not import is never pulled
  in (so unrelated harnesses can share a directory — the fixtures do), and the
  harness module itself never is (it defines `loop`, whose `runLLMTurn` is
  absent from the answerer's row, and GHC compiles an imported module whole).
  Resolving the type through the one shared module also fixes WHICH type it is,
  so constructor ids agree at the crossing.

Both halves are load-bearing: without the pin a wrong-typed answer traps, and
without the imports the model cannot name the type it is being asked for and
substitutes one that compiles. A harness that inlines its author types alongside
`loop` fails the second half — `SelfHarnessDriver::types_in_scope_hint` says so
in the retry rather than looping to the round cap. A node with no contract
compiles at `Finalize NoAnswer` and simply cannot finalize. Pinned by
`tests/finalize_type_pinning.rs`.

`askUser` re-prompts by RECURSION on a decode failure (no `Either` — the
retry is entirely Haskell-side): a bad submission genuinely re-suspends on a
fresh `AskUserWith`, not an error the driver observes. The driver services
this in [`SelfHarnessDriver::service_askuser_hole`]: present the form via the
operator gate, resume via [`Harness::answer_dialog`] (the same audited resume
path a mechanical `Dialog`/`Ask` answer uses — `answer_dialog` accepts
`AskUser` alongside them), and repeat while the resume keeps landing on
another `AskUser` suspension, reading the fresh pending hole via
[`Harness::pending_hole_full`] (the resume itself carries no outcome).
Bounded by `ASKUSER_MAX_REPROMPTS` (8) CONSECUTIVE re-presentations,
independent of and never counted against the model-round caps
(`ANSWERER_MAX_ROUNDS`/`LOOP_INFERENCE_CALL_CAP`) — a form resume is not a
model round, but left uncapped it composes with a non-interactive gate at EOF
(the default `StdinGate` returns an empty submission on EOF, not an error)
into an unbounded hot loop no round-based cap catches.

**The operator-input seam is [`selfharness::operator::OperatorGate`]**
— consume it, never redefine it there: `present_form(&FormShape) ->
serde_json::Value` and `await_continue()`, both SYNC-BLOCKING by design (the frozen
`OperatorGate` contract) even though the driver's turn loop is `async fn` and
`.await`s the `Harness` directly. `SelfHarnessDriver` holds
`gate: Arc<dyn OperatorGate>`, defaulting to
`StdinGate` (headless: reads one JSON line per form, one line per continue)
and overridable via `SelfHarnessDriver::set_gate` — a web/GUI implementation
parks on a channel instead. `between_loops_gate` (the human-clicks-continue
gate between loop iterations) is `gate.await_continue()` — no EOF-driven
close of the loop; the caller decides how a continue signal arrives. Every
gate call (`present_form`/`await_continue`) runs under `tokio::task::block_in_place`
so a web gate's channel park yields the tokio worker instead of stalling it.

The OUTER loop can present a form too: `outer_decls()` is `[RunLLMTurn,
AskUser]`, so an AUTHORED `loop` that `import`s `Tidepool.Form` and evaluates
`askUser` suspends on `AskUserWith`, serviced by
`SelfHarnessDriver::service_outer_askuser_hole` (the same gate, the same
`ASKUSER_MAX_REPROMPTS` bound, resuming the OUTER session via
`engine::json_answer_to_value`). This does NOT add `AskUser` to
`Tidepool.Harness`/`HarnessEff` (whose row stays `'[RunLLMTurn]`,
stale-but-unused): `Harness = M` and `askUser`'s `Member AskUser` constraint
unifies against the wider generated row.

### Outer fork/fanout servicing — a branch's exit is DATA at its position

An AUTHORED `loop` reaching for `runLLMTurnFork @T`/`runLLMTurnFanout @T`
suspends on `RunLLMTurn`'s own fork payload (no separate `Fork` decl needed —
`outer_decls()` has none), classified as `HoleRouting::Fork` and serviced by
`SelfHarnessDriver::service_outer_fanout` → `drive_fanout_child`: each child
gets a freshly-minted answerer realm on the shared outer machine, driven
CONCURRENTLY up to `set_concurrency_cap`, re-sorted to DECLARATION order
before assembly so completion order is never observable.

**Every verb that opens a window at a BRANCH POSITION answers an `Either`**
(PRD 21 locked decision 6,
`plans/self-iterating-harness/21-c3-exit-verb.md`):
`runLLMTurnFork @T :: Text -> M (Either InvocationExit T)`,
`runLLMTurnFanout @T :: [Text] -> M [Either InvocationExit T]`, and
`runLLMTurnBranch @T :: ContextRef -> Text -> M (Either InvocationExit (T, ContextRef))`
(the `Either` wraps the WHOLE pair — a window that never finalized has no
post-finalize prefix, so there is no honest `ContextRef` to sit beside the
failure). The two that do NOT open a branch position keep their bare answers:
`runLLMTurn @T`, answered in context by the same node, and `freezeContext`,
which is not a window at all. That asymmetry is documented at the declaration
(`tidepool_mcp::runllmturn_effect_def!`).

`runLLMTurnBranch` reaches it by a different route — `service_outer_branch` is
sequential and drives its child through `drive_answerer_to_finalize`, the round
loop it SHARES with the in-context `service_runllm_hole`. That loop returns
`Result<Result<TurnOutcome, InvocationExit>, DriverError>` and the two callers
differ in what they do with an exit, which is exactly the branch-position
distinction: the branch folds it as `Left`, the in-context hole collapses it
back into a hard failure (unchanged).

**The line, and it is the whole point of the shape.** A failure attributable
to ONE CHILD'S WINDOW — round exhaustion, ending on something that is not an
answer, that window's own provider call failing — comes back from
`drive_fanout_child` as `Ok(Err(exit))` and is folded as `Left exit` at that
child's branch position, so its siblings' finished answers survive. A failure
of the MECHANISM — fan cardinality, `Either`/list assembly against the
`DataConTable`, session bookkeeping, the per-loop inference-call runaway cap,
and a child that finalized a CLOSURE (it DID answer; this driver cannot carry
it) — still hard-fails the turn. Laundering a broken mechanism into "the model
failed" would be a false receipt. The nesting of
`Result<Result<Value, InvocationExit>, DriverError>` IS that contract: outer =
mechanism, inner = the window.

`engine::build_child_answer_value`/`build_invocation_exit_value` construct the
`Left`/`Right`/`Exit*` values against the turn's own table with
`build_list_value`'s loud-failure discipline (a missing constructor is a hard
error, never a default). The constructors are present by construction: a
fork/fanout site head-swaps to a `*Sited` sibling whose top-level type mentions
`Either InvocationExit a`, and extract's `collectTransitiveDCons` seeds from
reachable top-level binders' types.

The NESTED path (`Harness::answer_fork`/`answer_fanout`, the general Agent
stack and `drain_answerer_fork`) shares `HoleRouting::Fork` with
`Tidepool.Fork`'s `fork`/`forkAll`, which still answer a bare `T`/`[T]` — so
the routing carries `engine::ForkSource` and `Harness::wrap_fork_answer` wraps
in `Right` only for a `runLLMTurn`-sourced hole. That path produces no `Left`
yet: a child failing there still hard-fails the fan through
`drive_answerer_to_value`'s escalation ladder.

One consequence worth knowing before writing a harness: `InvocationExit` lives
in the per-fragment generated `Tidepool.Effects`, so the cross-row bind guard
refuses an `Either InvocationExit T` as a cross-turn session VALUE BIND (same
rule that already covered `Schema`). Project at the bind —
`steps <- either (\_ -> []) id <$> runLLMTurnFork @[Int] "…"`.

Gates: `tests/outer_fanout.rs` (fork/fanout) and
`tests/companion_context_ref.rs` (branch).

### One session: attached realms, closure delivery, machine rotation

Pre-collapse, the outer `render`/`loop` session and each loop's answerer
Agent were separate resident sessions, and a finalized answer crossed between
them by BRIDGING to a JSON-shaped `Value` — a closure could not survive that
crossing. The one-session collapse (`plans/one-session.md`) removes the
boundary: the outer session is the tree's one node-less, registry-owned
session (`SelfHarnessDriver::bootstrap` calls `Harness::adopt_session`, which
is `NodeTree::adopt_session` — the driver holds only the `SessionId`), and
every per-loop answerer node ATTACHES to that same session instead of getting
its own (`Harness::force_attached`, not `Harness::force`). An attached node
never OWNS its session (`NodeTree::node_owns_session` is false for it); its
turns run as a REALM on the shared machine, minted per loop
(`SelfHarnessDriver::set_node_realm`) and applied to the machine by
`run_checked_out` before every turn (see Machine lifecycle above), so an
answerer's parked frames and any values it produces are born directly in the
loop's own heap. Retiring the answerer at loop end
(`SelfHarnessDriver::retire_answerer` → `Harness::terminate_node`) is that
realm's SCOPE EXIT (`close_realm`), never session/slot removal — the shared
outer session outlives every answerer node it hosts. Outer `render`/`loop`
fragments and every answerer turn go through the one checkout discipline via
`Harness::with_session` (a thin `checkout_run` + restore-with-reported-holes
wrapper for the node-less shared session).

**Finalize delivery is by HANDLE, not by bridge, when the payload is a
closure.** A data answer still crosses as a bridged `Value`
(`Harness::take_finalized_value_keep_open`); a closure (or any value that
would sentinel under the eager bridge) is taken as a `ValueHandle`
(`Harness::take_finalized_handle_keep_open`, gated by
`Harness::finalize_is_closure`) and delivered into the loop's parked
`runLLMTurn` continuation via `ResidentSession::resume_handle` — the payload
pointer feeds the resumed continuation verbatim, on the same heap, no
materialization. This is the mechanism behind `runLLMTurn @(State -> State)`
working end to end — including closures NESTED in a product (a record of
functions), routed by a DEEP sentinel scan: the answerer finalizes it, the
loop applies it directly. And the shared session carries the LIVING DECL
PLANE (`SelfHarnessDriver::open_outer_plane`): pure top-level declarations a
model defines persist BY NAME across loops AND across machine rotations
(the plane is source-side state; `take_lib` transfers it into the rotated
machine), validated against the effects-dir-free include so an effectful
decl fails at define time (the structural pure-decls guard), and NEVER on
the authored render/loop compiles' include path (pillar D). Standing
acceptances, all in `tests/selfharness_fn_finalize_spike.rs`: the
`State -> State` edit, the record-of-functions delivery, and
`living_helper_survives_loop_boundary_and_rotation`. Restart persistence of
the plane (decl-log disk reload) is future work; heap VALUES still die at
rotation, enumerated.
The scoped-stack caveat in Replay above still holds unchanged: the answerer
row is all-suspending, so it produces no `Event::Effect` regardless of
whether its session is owned or attached.

**Machine lifetime is bounded by rotation, not immortality.** Because
cross-loop closures are now the point, the shared machine is not rebuilt
every loop — it is measured every loop boundary
(`SelfHarnessDriver::machine_maintenance` emits `Event::MachineStats`,
carrying `HeapStats::fragments`) and ROTATED at a quiescent boundary once
`stats.fragments` reaches `TIDEPOOL_MACHINE_FRAGMENT_CEILING` (default 4096):
a fresh machine is adopted under the SAME `SessionId`
(`Harness::replace_session`), durable `State` flows through the checkpoint
exactly as every loop already threads it, and whatever cannot reconstruct
(session-plane bindings, including closures) is enumerated into
`Event::MachineRotated` and the next render's legible-loss note — never
silently dropped. A non-quiescent machine (parked holes outstanding) at the
ceiling refuses the loop with a legible error rather than rotating under a
live suspension. CI oracle:
`machine_rotation_between_cycles_preserves_durable_state`.

## Tailing the durable log

Two DISTINCT jsonl streams live under `<cache>/selfharness/` (paths from
`selfharness::persistence`):

- **`transcript.jsonl`** (`default_transcript_path`, written by `JsonlObserver`
  over the `Observer` seam) — the LOOP-level story, and the whole input to the
  telemetry fold (first-compile success rate, retries-per-hole —
  `tests/dogfood_observability.rs`):
  `LoopBoundary`; `RunLLMTurnHole{site,ty,prompt}` (the hole's human-facing
  ask, not just its site/type); `TurnStart`/`TurnEnd` (node ids only);
  `AnswererRound{node,site,round,error}` — one line per answerer round while
  servicing a `runLLMTurn` hole, `round` 1-based WITHIN that hole's servicing,
  `error` the UNTRUNCATED GHC error on a failed compile or `null` on a
  compiled round — the fold groups these by `site`; `Finalize{node,value}`
  (the finalized answer, rendered to JSON text, not just that one arrived);
  `FormPresented{source,shape}`/`FormSubmitted{source,submission}` (an
  `askUser` form's shape and the operator's reply — `source` distinguishes a
  nested answerer's own form from one the AUTHORED OUTER loop raised
  directly); `OuterCompile{label,source}` (the OUTER session's own `render`/
  `loop` fragment compiles — `crate::log::Event::TurnStart` never covers
  these, the outer session is not a tree node); `CompactionTrigger{summary,…}`.
  One line per driver `Event`.
- **`log-<epoch>.jsonl`** (parent directory from `default_log_path`, the
  durable per-NODE `crate::log` written by the answerer `Harness`'s
  `LogWriter`) — the fine-grained story:
  `Forced`, `TurnStart{source}` (the EXTRACTED executed Haskell, so
  `jq -r 'select(.ev=="turn_start").source'` over the newest `log-*.jsonl` prints
  the exact blocks the answerer ran — also surfaced at console INFO, not just
  the durable line), `TurnExtracted{asks,bound}`
  (what extract said this turn's holes/binds ARE — the `asks.json` site → type
  table and a value-plane bind's bound name/type, when either is non-empty),
  `TurnDelta` (the full model reply), `HolePublished`/`HoleConsumed` (each
  `askUser`/`finalize` suspension + answer), `NodeDone`. `Event::Effect`
  appears here only for a node whose stack has base effects — the scoped
  answerer/outer stacks have none, so effect activity shows as
  `HolePublished`/`HoleConsumed`, not `Effect` (see Replay).

The production binary (`tidepool-web/src/bin/tidepool-selfharness.rs`) does
NOT reuse one fixed `log.jsonl`: `LogWriter` refuses to overwrite an existing
run's log, so each boot mints its own `log-<epoch>.jsonl` sibling under
`<cache>/selfharness/` — tail the NEWEST one, e.g.
`tail -f $(ls -t <cache>/selfharness/log-*.jsonl | head -1)`. A caller that
wants one stable, reused filename (a direct test, say) can still boot the
answerer `Harness` with `LogWriter::create(&default_log_path(), &header)` to
land a fixed `log.jsonl` on that exact path; the driver writes
`transcript.jsonl` via a `JsonlObserver` at `default_transcript_path()`
regardless. `tail -f` either. A `timing` DEBUG stage's `node`/`round` fields
render as words (`timing::render_node`/`render_round`) — `"bootstrap"`/`"-"`
for `NO_NODE`/`NO_ROUND`, never a raw `u64::MAX`.
