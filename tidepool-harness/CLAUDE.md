# tidepool-harness — typed-yield session harness (R0 build)

A new frontend over the eval substrate — NOT a retrofit of tidepool-repl
(which keeps its parked-thread mechanism unchanged). Plan + segment specs:
`plans/harness-r0/`; cross-segment contracts:
`plans/harness-r0/00-scaffold/contracts.md`; frozen shapes:
`plans/harness-r0/FREEZES.md`.

Module map:
- `tree`/`forcing` — `NodeId`/`NodeState`/`HoleId`/`SiteId`, forcing badges,
  and `NodeTree<M>`: parent/child structure + per-node lifecycle state,
  backed by the durable event log. Generic over a machine handle `M` and
  backed internally by a `SessionRegistry<M>` (see Machine lifecycle below).
- `registry` — `SessionRegistry<M>`: the `Idle | Running | RunningChild |
  Suspended` slot machine + atomic checkout/restore, including nested-child
  checkout (segment 40, landed — `checkout_child`/`RunningChild`).
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
  + `asks.json` sidecar) via `tidepool-extract`, independent of
  `tidepool-runtime`'s caching compile (turns are one-shot, no cache needed).
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
- `ui`/`uiof` — the `Ui` eDSL wire mirror (Haskell `Tidepool.Ui`'s contract
  partner) and `uiOf` (server-derived mechanical forms from a compiled
  `DataConTable`, no Haskell Generic machinery).

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
on the blocking pool via `Harness::run_checked_out`, and restores it
`Idle`/`Suspended{hole}` based on the machine's OWN post-call state
(`Session::is_idle`/`pending_continuation`) — not a guess from the turn's
domain result, so an errored `run`/`resume` still restores correctly. A
`checkout_child` (nested child run against a suspended continuation — the
`run_child` discipline: an answer value crosses via a non-consuming child run
against the TARGET's own suspended session) keeps that session `Suspended` on
the SAME hole throughout, whether the child run succeeds or fails.

`Checkout` is panic-safe: if a checkout is dropped without an explicit
restore (a panic unwinding between checkout and restore, before the machine
was ever moved off the checkout via `take()`), `Drop` restores it `Idle`
rather than leaving the registry slot wedged `Running` forever. The one case
`Drop` cannot cover is a machine already moved onto the blocking pool via
`take()`: if that task panics (`JoinError`), the machine is genuinely gone —
`run_checked_out` calls `Harness::terminate_node` instead of trying to
restore a machine it does not have.

`Harness::terminate_node` is the ONE retirement path: idempotently
terminalize the tree entry (`NodeTree::node_cancelled`, skipped if already
`Done`/`Cancelled`), remove the session from the registry
(`SessionRegistry::remove`, dropping the machine), and remove the node's
`convos` entry. `cancel`, a failed fork/fanout child's cleanup, the
`JoinError` path above, and the self-iterating harness's `retire_answerer`
all retire a node through it — there is no second way to retire one. A busy
node (`CheckoutError::Running`/`RunningChild`) surfaces as
`HarnessError::TurnInFlight`, never `NoSession` — that variant is reserved
for a node that genuinely has no session (never forced, or already
terminated).

`convos: Mutex<HashMap<NodeId, NodeConvo>>` still holds everything a session
checkout doesn't: the transcript, the pending hole, per-node framing, the
answer contract, the turn lease. A read that needs the session's own state
WITHOUT checking it out (decl-plane context for a session-aware compile,
observatory heap stats) goes through `SessionRegistry::peek`, which succeeds
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
  log at startup would be dual lifecycle machinery, the thing this lane
  exists to remove. `golden_path`'s crash-replay assertion (a killed
  process's log folds back to the terminal tree) is what pins this contract.

**`Event::Effect` IS written by the live turn loop; effect-response
SUBSTITUTION on replay is what remains out of scope.** The writer
(`NodeTree::effect` ← `Harness::flush_effects`, which drains the node's
`effect_trace` after each `run_block`/`answer_*`) is wired
into the live path: every turn that dispatches a HANDLED (non-suspending)
effect produces one `Event::Effect{req,resp}` per effect. A SUSPENDING effect
(`Ask`/`AskUser`/`RunLLMTurn`/`Finalize`) never reaches a handler, so it logs
as `HolePublished`/`HoleConsumed`, not `Effect`. **Scoped-stack caveat:** the
self-iterating harness's answerer (`[AskUser, Finalize]`) and outer loop
(`[RunLLMTurn, AskUser]`) declare ONLY suspending effects — no base
`Console`/`Fs`/`Http`/… — so `flush_effects` runs but drains an empty trace:
those nodes produce NO `Event::Effect` BY CONSTRUCTION (that absence IS the
capability boundary — the answerer structurally cannot run a shell/file/net
effect). A general Agent node (full base-effect row) does produce them. What
is still OUT OF R0 SCOPE is a READER that substitutes recorded effect
responses back into a resumed session on restart — a node that suspended
after running handled effects, then restarted and resumed, would RE-EXECUTE
them live rather than replay recorded responses. The record side is live; the
replay side is reserved.

## Rules inherited from the plan

Forcing events are the only work-begins mechanism (consent integrity audits
to literal zero — `NodeTree::force` is the only transition out of `Thunk`,
and it logs `Event::Forced{actor}` before any session exists); teasers are
harness-generated only (`forcing.rs::derive_teaser`).

## Self-iterating harness — the answerer row + the `AskUser` operator gate

The self-iterating harness's answerer Agent (`selfharness::driver::answerer_decls`)
compiles against `Eff '[AskUser, Finalize]` — two decl-only effects, disjoint
from the general Agent stack's `standard_decls()` (which keeps `Ask`,
`RunLLMTurn`, and every base effect untouched; `AskUser` never appears
there). `AskUser` (`tidepool_mcp::askuser_decl`) is a brand-new effect, not a
rename of `Ask`: `Ask` suspends `ask schema prompt` to the CALLING LLM AGENT
with a JSON Schema; `AskUser` suspends `askUserRaw :: Value -> M Value` (the
raw wire escape; the typed surface authors write is `askUser :: Form a -> M
a`, `Tidepool.Form`) to a HUMAN OPERATOR with a typed [`FormSpec`]
(`selfharness::operator`), routed by CONSTRUCTOR NAME (`AskUserWith`) in
[`engine::classify_hole`] — no JSON-key probing. `Tidepool.Form` is
auto-imported into a turn's preamble whenever `AskUser` is in the compiling
decl list (`tidepool-mcp`'s `pragmas_and_imports`/`session_decl_module_env`);
it depends on `askUserRaw`, so it is REACHABLE ONLY on the answerer stack, not
the general eval/Agent surface.

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
  binds it), `a` free (so the template's `toJSON _r` defaults it rather than
  demanding `ToJSON T`), `Member` a real constraint (so the dictionary rides as
  the leading value arg `Translate.hs` re-applies when head-swapping to
  `finalizeSited`). Extract is untouched by the indexing — `asks.json` records
  the site type exactly as before, and `v` is erased in Core, so `FinalizeWith`
  keeps its arity and `Finalize` its positional union tag.

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
  GHC says it by name. There is no "unpinned finalize" any more: the row admits
  exactly one answer type, so the old failure mode (any `v` compiles, then
  crosses in-heap into a `T`-typed continuation and case-traps past every
  check) is not expressible.
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
— consume it, never redefine it there: `present_form(&FormSpec) ->
Submission` and `await_continue()`, both SYNC-BLOCKING by design (the frozen
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

## Tailing the durable log

Two DISTINCT jsonl streams live under `<cache>/selfharness/` (paths from
`selfharness::persistence`):

- **`transcript.jsonl`** (`default_transcript_path`, written by `JsonlObserver`
  over the `Observer` seam) — the LOOP-level story: `LoopBoundary`,
  `RunLLMTurnHole`, `TurnStart`/`TurnEnd`/`Finalize` (node ids only),
  `CompactionTrigger{summary,…}`. One line per driver `Event`.
- **`log.jsonl`** (`default_log_path`, the durable per-NODE `crate::log`
  written by the answerer `Harness`'s `LogWriter`) — the fine-grained story:
  `Forced`, `TurnStart{source}` (the EXTRACTED executed Haskell, so
  `tail -f log.jsonl | jq -r 'select(.ev=="turn_start").source'` prints
  the exact blocks the answerer ran), `TurnDelta` (the full model reply),
  `HolePublished`/`HoleConsumed` (each `askUser`/`finalize` suspension +
  answer), `NodeDone`. `Event::Effect` appears here only for a node whose stack
  has base effects — the scoped answerer/outer stacks have none, so effect
  activity shows as `HolePublished`/`HoleConsumed`, not `Effect` (see Replay).

A caller boots the answerer `Harness` with `LogWriter::create(&default_log_path(),
&header)` to land `log.jsonl` on this path; the driver writes `transcript.jsonl`
via a `JsonlObserver` at `default_transcript_path()`. `tail -f` either.
