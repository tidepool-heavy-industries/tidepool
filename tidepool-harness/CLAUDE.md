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
  name.
- `compile` — turn compilation (Haskell source → `CoreExpr` + `DataConTable`
  + `asks.json` sidecar) via `tidepool-extract`, MEMOIZED through
  `tidepool_runtime::cache` (see Compile memo below).
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
- `synopsis` — the names-only `type_synopsis` a hole card's shape line reads,
  derived from a compiled `DataConTable` (constructor/selector NAMES only —
  the table has no field TYPES, so this is honestly shallow, never a form).

## Compile memo — one content-addressed cache, no cache-free path

`compile.rs` memoizes every turn compile. There is no cache-free path, no
bypass flag, and no second mechanism. The full keying spec, the correctness
hazards it answers, and the safety argument for sharing one memo across test
processes are in `plans/compile-memo.md`; what a reader here needs:

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
  rejoin at `compile::assemble`, so observational identity is a property of
  the code shape. The one deliberate difference: a hit records no
  `extract_spawn` timing stage and no `extract.*` phases, because nothing was
  spawned.
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

## Invariants

Forcing events are the only work-begins mechanism (consent integrity audits
to literal zero — `NodeTree::force` is the only transition out of `Thunk`,
and it logs `Event::Forced{actor}` before any session exists); teasers are
harness-generated only (`forcing.rs::derive_teaser`).

## Self-iterating harness — the answerer row + the `AskUser` operator gate

The self-iterating harness's answerer Agent (`selfharness::driver::answerer_decls`)
compiles against `Eff '[AskUser, Fork, Finalize]` — decl-only effects, disjoint
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

### The answer contract — `finalize` is pinned by the ROW

An answerer turn does not compile against a polymorphic `finalize`. While a
node is answering a typed hole it carries an `AnswerContract` (set per hole by
the driver via `Harness::set_answer_contract`, since the per-loop answerer node
is reused across holes whose types differ), and its turns compile with:

- **`Finalize` instantiated at the hole's type IN THE ROW.** `Finalize` is
  type-indexed — `data Finalize v a where FinalizeWith :: Int -> v -> Finalize
  v a`, `finalize :: forall v a effs. Member (Finalize v) effs => v -> Eff effs
  a` — so a turn answering a `Decision` hole compiles against `'[AskUser, Fork,
  Finalize Decision]` and `Member (Finalize Decision)` IS the pin. Canonical
  freer-simple, the same shape as `State s`. A wrong-typed answer is an
  ordinary GHC error naming the row (`'Finalize Text' is not a member of the
  type-level list '[AskUser, Fork, Finalize Decision]'`), which the
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
- **`log.jsonl`** (`default_log_path`, the durable per-NODE `crate::log`
  written by the answerer `Harness`'s `LogWriter`) — the fine-grained story:
  `Forced`, `TurnStart{source}` (the EXTRACTED executed Haskell, so
  `tail -f log.jsonl | jq -r 'select(.ev=="turn_start").source'` prints
  the exact blocks the answerer ran — also surfaced at console INFO, not just
  the durable line), `TurnExtracted{asks,bound}`
  (what extract said this turn's holes/binds ARE — the `asks.json` site → type
  table and a value-plane bind's bound name/type, when either is non-empty),
  `TurnDelta` (the full model reply), `HolePublished`/`HoleConsumed` (each
  `askUser`/`finalize` suspension + answer), `NodeDone`. `Event::Effect`
  appears here only for a node whose stack has base effects — the scoped
  answerer/outer stacks have none, so effect activity shows as
  `HolePublished`/`HoleConsumed`, not `Effect` (see Replay).

A caller boots the answerer `Harness` with `LogWriter::create(&default_log_path(),
&header)` to land `log.jsonl` on this path; the driver writes `transcript.jsonl`
via a `JsonlObserver` at `default_transcript_path()`. `tail -f` either. A
`timing` DEBUG stage's `node`/`round` fields render as words
(`timing::render_node`/`render_round`) — `"bootstrap"`/`"-"` for
`NO_NODE`/`NO_ROUND`, never a raw `u64::MAX`.
