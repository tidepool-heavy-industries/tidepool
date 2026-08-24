# Resident-session suspension kernel — design (#22)

Status: DESIGN ONLY, for operator review. No production code in this change.
Written against tip `6efd79d5` (post `a95eac61`, the #20-step-1 suspension
decode migration). Line numbers cited below are read-aids as of this commit,
not authoritative — per the wave runbook's own rule, relocate every claim by
symbol name if the tree has moved.

## TL;DR

`tidepool-repl` and `tidepool-harness` both implement "a resident machine
parks on a typed hole, something external resolves it, the machine resumes,"
but they do it on **two different underlying suspend mechanisms** that are
already flagged in the codebase as a temporary fork, not two permanent
designs:

- repl (and one-shot eval) drive `PersistentSession`'s **slot path** — one
  `suspended_continuation` on the machine itself, single-hole.
- harness drives `PersistentSession` through **`ResidentSession`**, the
  **parked path** — a continuation *registry*, multi-hole.

`tidepool-runtime/src/session/persistent.rs:17-33`'s own module doc says the
slot path "remains the live mechanism for the repl and one-shot eval lanes;
on the harness lane it is legacy, superseded by the parked path, and its
deletion is gated on the parked path's production soak" — citing
`plans/one-session.md`'s **Phase 6** ("repl/one-shot conversion + slot
deletion"), which `plans/README.md:120-124` confirms is "not currently in
flight," parked behind a production-soak gate.

**This is the load-bearing fact for #22.** A kernel that unifies suspend/
resume orchestration is either (a) sequenced *after* Phase 6, in which case
both crates already share one suspend mechanism and "the kernel" is mostly
extracting the orchestration vocabulary that already lives on
`ResidentSession`/`ResidentHole` into a form the repl's now-parked-path
session can consume too — or (b) sequenced *before* Phase 6, in which case
the kernel has to abstract over two genuinely different machine-level
suspend mechanisms, which is a harder and more speculative piece of
engineering than anything #22's brief describes. §5 and the open questions
below make this the first thing the operator needs to rule on; everything
else in this doc is written to be correct under either answer, but the
migration order differs sharply between them.

Independent of that fork, the registry/ownership layer is **already
unified**: both crates instantiate the same `tidepool_runtime::session::
registry::{SessionRegistry, Slot, Checkout}` (harness at `HoleId`, repl at
`ContinuationId` via `SingleSlot`). The `#22` design is not "should we share
a registry" — that shipped as the registry-capstone wave (`plans/
registry-capstone.md`, landed 2026-08-22). It is "should we also share the
resume-dispatch shape, the hole-obligation typing, and the error taxonomy
sitting on top of that registry" — which today are two independent,
differently-evolved designs.

---

## 1. Side-by-side survey

### 1.1 State machines

**repl** (`tidepool-repl/src/session.rs`, driving `PersistentSession`'s slot
path directly):

- Turn-level: `TurnStep::Completed(TurnOutcome) | Suspended(AskRequest)`
  (`session.rs:82-85`).
- Item-level (one item inside a `session_run` block): `ItemStep::
  Done(TurnOutcome) | Suspended(PendingTail, AskRequest)` (`session.rs:95-101`).
- **Single-hole by construction.** `Session::suspended: Option<SuspendedTurn>`
  (`session.rs:182`) — `Some` exactly while the session is suspended on one
  `ask`. `SuspendedTurn { tail: PendingTail, cursor: SuspendedBlockCursor }`
  (`session.rs:331-334`) is consumed *wholesale* by `Session::reenter`
  (`session.rs:657-677`) specifically so a resume can never take the tail
  without the cursor (or vice versa) — the doc at `session.rs:326-330` names
  this as the exact bug a two-field split used to allow.
- The five run-path shapes (`PendingTail::PlainEval/Bind/MultiBind/
  Reference/BareExpr`, `session.rs:283-295`) each carry only the state their
  own `finish_*`/resume pair needs, and each has its own machine-level
  `resume_*` sibling on `PersistentSession` (`resume_with_table`/
  `resume_session`/`resume_bind`/`resume_bind_projected`/`resume_bind_render`,
  `persistent.rs:439-750`) — **five separate resume entry points**, dispatched
  by `Session::resume_item`'s match over `PendingTail` (`session.rs:1044-1083`).
- Block-loop re-entry: `BlockCursor` (`session.rs:379-399`) is stowed as
  `SuspendedBlockCursor { cursor, pending_item }` (`session.rs:349-352`) —
  "there is no path that produces a cursor with a pending item the type
  doesn't know about."

**harness** (`tidepool-runtime/src/session/resident.rs`'s `ResidentSession`,
consumed by `tidepool-harness/src/harness.rs`):

- Turn-level: `ResidentOutcome::Completed{output,result} | Suspended{output,
  hole,request}` (`resident.rs:334-350`).
- **Multi-hole by construction.** `ResidentSession::parked: Vec<(String,
  ContinuationId)>` (`resident.rs:445-449`), insertion-ordered, resumable by
  identity in *any* order (the machine's continuation registry imposes none).
  One session can carry N parked holes simultaneously — the one-session
  collapse's concurrently-driven attached-answerer realms sharing one
  machine is the production case that needs this.
- **One resume entry point.** `ResidentHole::Plain(PlainHole) |
  Binding(BindingHole)` (`resident.rs:253-256`) carries its own completion
  obligation *on the token itself*: `ResidentSession::resume(hole,
  answer)` (`resident.rs:1603-1616`) is documented as replacing "the old
  `resume`/`resume_bind` split" precisely because that split let "a binding
  hole silently resolve through the plain path, completing the machine side
  while the value-plane bind it owed never materialized" (`resident.rs:
  220-235`). There is exactly one machine-level `resume_parked` underneath
  it (`reenter`, `resident.rs:1632-1697`), not five.
- Harness's own layer on top adds a second, orchestration-scoped hole
  identity: `Harness::pending_suspensions: Mutex<HashMap<(SessionId,HoleId),
  PendingSuspension>>` (`harness.rs:545`), where `PendingSuspension`
  (`harness.rs:280-309`) carries the routing (`ClassifiedSuspension`), the
  raw request `Value`, the `ResidentHole` token, and the compile artifacts
  (`suspend_table`/`suspend_asks`) needed to bridge an answer. This is
  deliberately *separate* from the registry's own hole set — "the same
  'authoritative set + external domain metadata' split the registry itself
  draws around `Slot`" (`tidepool-harness/CLAUDE.md`, "Suspension metadata").
  A node suspends on **at most one** hole at a time (`resident_hole` is
  always *replaced*, never accumulated — `harness.rs:281-289`); the
  registry's multi-hole set is multi because it spans *multiple nodes*
  sharing one session, never because one node juggles several holes.

Both crates therefore already agree on one design principle — "the
obligation and the routing travel with the token, never as an external flag
a caller must remember to consult" — but repl encodes it as **which of five
functions you call**, harness encodes it as **which variant a single
function's argument is**. That is a real, generalizable difference (§2).

### 1.2 Ownership

Identical mechanism, different instantiation width — this axis is already
solved by the registry-capstone wave, not open in #22:

- `tidepool_runtime::session::registry::{SessionRegistry<M,H>, Slot<M,H>,
  Checkout<'_,M,H>, CheckoutError<H>}` (`registry.rs`) is the one home. `Idle
  (M) | Running{holes} | Suspended{machine,holes} | Wedged{since}`
  (`registry.rs:107-130`), epoch-guarded (`registry.rs:41-52`), panic-safe
  `Drop` restoring the carried hole set (`registry.rs:543-564`).
- **harness**: `tidepool_harness::registry::{SessionRegistry<M>, Checkout<'_,
  M>, CheckoutError}` (`tidepool-harness/src/registry.rs:11-13`) fixes `H =
  crate::tree::HoleId` and embeds the registry inside `NodeTree<M>`
  (`tidepool-harness/src/forcing.rs:117-120`) — the keyed, full-generality
  instantiation: N sessions, N holes each.
- **repl**: `tidepool_runtime::session::registry::SingleSlot<M,H>`
  (`registry.rs:659-777`) restricts the *same* registry to "at most one
  entry, no id parameter," instantiated at `H = ContinuationId`
  (`tidepool-repl/src/manager.rs:82-86`) inside `SessionManager`
  (`manager.rs:113-118`).
- Neither crate hand-rolls a second lifecycle enum. repl's pre-promotion
  `state.rs`/`SessionState` is deleted (`tidepool-repl/CLAUDE.md`, "One
  registry, no second lifecycle truth"); the registry's `Slot` is the only
  truth on both sides.
- **Policy divergence, correctly kept out of the shared primitive**: repl's
  `SessionManager::admit_run` (`manager.rs:247-262`) refuses a fresh
  `session_run` while `Suspended` — documented as *repl's own contract*, not
  the registry's, since the shared `checkout_run` itself permits a run over
  a suspended slot (the multi-hole story harness needs). This is exactly the
  shape a kernel-level "policy hook" should generalize (§3), not something
  either side should lose.

### 1.3 Resume typing

Covered above (§1.1); restated as the one-line comparison for §2: repl =
5 typed tails × 5 machine resume entries, dispatched externally by
`match`; harness = 1 obligation-carrying token × 1 machine resume entry,
dispatched internally by the token's own variant.

On top of the machine-level token, harness's *orchestration*-level typing
(what a suspended request's payload *means*) is now schema-generated
(post-`a95eac61`): `RosterRequest` (`tidepool-harness/src/engine.rs:454-468`)
is a hand-composed sum over generated per-effect request types (nine fresh
via `tidepool_protocol::effects::suspension_roster` into
`tidepool-harness/src/generated/`, four reused from `tidepool-handlers`'s
already-generated `WorktreeReq`/`RepoEventReq`/`ExecReq`/`JournalReq`).
`decode_roster` (`engine.rs:482-535`) tries each member's `FromCore` in
turn; `classify_hole` (`engine.rs:588+`) turns the matched member into the
orchestration-level `SuspensionRouting` enum (`engine.rs:227-328`, 9
variants: `RunLLMTurn`, `Fork{source:ForkSource}`, `AskUser`, `Note`,
`ReadState`, `Subagent`, `OuterEffect(OuterEffectKind)`, `Finalize`,
`Green`, `Ask`). The match over `RosterRequest` is exhaustive by
construction — adding a roster member without a `classify_hole` arm is now
a compile error, closing the "`RepoEventAwait` bug class" the migration's
own commit message names (a new suspending effect silently falling through
to a plain `Ask`).

repl has no equivalent roster because it has exactly one suspending effect
(`Ask`) end to end; its decode (`tidepool_runtime::session::engine::
extract_ask_request`, `tidepool-runtime/src/session/engine.rs:1211-1235`) is
a small hand-rolled constructor-name match (`"AskWith" | "RunLLMTurnWith"`)
— structurally the *same* pattern harness's `classify_hole` had *before*
`a95eac61`, just with a roster of one so drift has never bitten it. See §6.

### 1.4 Checkpoint interaction

Neither side actually restores a live suspension across a process restart
today; they fail differently, which matters for the kernel's error/recovery
story:

- **repl has no durability layer at all** for the ask lifecycle. A process
  restart loses the session, the machine, and any pending `ask`,
  unconditionally — there is no log, no checkpoint. The only in-process
  safety net is the reaper (§1.5).
- **harness has a durable JSONL log** of hole transitions
  (`Event::HolePublished`/`Event::HoleConsumed`, written by
  `NodeTree::hole_published`/`hole_consumed`, `forcing.rs:322-351` and
  `381-405`) — but this is an *audit trail*, not a restart-recovery
  mechanism. Separately, `selfharness::persistence::Checkpoint`
  (`tidepool-harness/src/selfharness/persistence.rs:149-196`) is the actual
  restart-recovery artifact the driver restores from
  (`SelfHarnessDriver::restore`), and it captures only **loop-boundary**
  state — `state: Json`, `generation`, `iteration`, `harness_source` — with
  no representation of an in-flight suspended hole at all.
  `tidepool-harness/CLAUDE.md`'s Replay section is explicit that
  `fold_tree_state` (which *could* reconstruct a suspension from the log) is
  "an inspection tool... NOT the startup recovery path."

Net: a harness node suspended mid-turn when the process dies does not come
back suspended on restart — the loop resumes from the last committed
loop-boundary checkpoint and re-drives that cycle, per "at-least-once
semantics" (`tidepool-harness/CLAUDE.md`, "Restart safety is a uniform
rule"). repl has the same practical outcome (the suspension is gone) via a
blunter mechanism (nothing survives at all). **The kernel should not invent
a new persistence story here** — that is explicitly `#21`'s scope
(`plans/persistence-versioning-design.md`, running in parallel), and this
doc's crate/seam recommendation (§3-4) should be checked against whatever
#21 lands rather than pre-empting it.

### 1.5 Error paths

**repl** — three-way, *structurally* distinguished resume-rejection surface:
`SessionManager::suspension_for(cont_id)` (`manager.rs:189-207`) returns
`Err(None)` (no suspension at all), `Err(Some(pending_cont_id))` (suspended
on a *different* continuation — the caller gets the pending id back as
data, not just a string), or `Ok((schema, captured))` (match — the reply is
then schema-validated *before* the continuation is consumed, so a bad
`session_resume` payload is retryable without losing the hole). `session_
reset` unconditionally folds abort into reset (`SessionManager::remove`,
`manager.rs:370-375`). A **TTL reaper** (`tidepool-repl/src/server.rs:
1053-1124`) is the one time-based cleanup mechanism in either crate:
`reap_once` aborts an abandoned `Suspended` past `suspended_ttl` via
`abort_abandoned` (driving a real `abort_turn` through the machine, so the
session comes back genuinely `Idle` with everything already accumulated
intact), or removes a stale `Wedged` past `wedged_ttl`. `Wedged` itself is a
first-class, terminal `Slot` variant visible to *every* future caller
(registry-capstone's fix over the pre-promotion design, where only the
original caller's own stale handle ever saw "wedged").

**harness** — richer *typed* error taxonomy, no time-based cleanup at all:
`HarnessError` (`harness.rs:75-122`): `NoSession`, `TurnInFlight` (busy),
`NotSuspended`, `RoutingMismatch{node,routing,actual}` (e.g. `answer_dialog`
called against a `Finalize` hole), `SessionMismatch{node,detail}` (wraps a
registry `CheckoutError::NotSuspended`/`WrongHole`/etc. via `HarnessError::
from_checkout`, `harness.rs:130-148` — the one place a checkout refusal
becomes a node-scoped error), and `BorrowedRootOnBindingHole(NodeId)` (a
green-thread handle delivery that cannot honor a `Binding` hole's
materialize obligation — refused rather than silently dropped, the exact
failure mode `ResidentHole`'s design (§1.1/1.3) exists to make impossible
*except* at this one borrowed-delivery seam). `ClassifyError`
(`engine.rs:346-384`) post-`a95eac61` distinguishes `UnsupportedConstructor`
(the roster itself is missing a member — a code bug) from `Decode{
constructor,detail}` (the roster matched but the *value* was malformed — a
genuinely bad payload) — a distinction that did not exist before the
migration. **There is no TTL reaper and no `Wedged`-equivalent state on the
harness's own hole map** (confirmed by grep — `pending_suspensions` has no
time field at all); an orphaned suspension is cleaned up only by explicit
`Harness::terminate_node` (`harness.rs:3472-3539`) — cancellation, a failed
fork/fanout child, a panicked-turn `JoinError`, or the self-iterating
driver's own retirement calls. A hole nobody ever answers (no operator
response, no automated resume, no cancellation) sits suspended **forever**
with nothing watching it.

`RoutingMismatch`'s structural loss vs. repl: `CheckoutError::WrongHole{
session, attempted, parked}` (`registry.rs:88-92`) carries the *actual*
parked-hole list as structured data, but by the time it surfaces through
`HarnessError::from_checkout` it is flattened into `SessionMismatch{node,
detail: String}` (`harness.rs:143-146`) — a caller can display it, but
cannot programmatically ask "what *was* pending" the way repl's `Err(Some
(pending))` lets a caller do without a second read.

---

## 2. Best-parts table

| Dimension | Winner | Why |
|---|---|---|
| Registry/ownership mechanism | **Tie — already unified** | Same `SessionRegistry`/`Slot`/`Checkout` primitive (§1.2); nothing to design here, only to preserve. |
| Resume dispatch shape | **harness** | One `resume(hole, answer)` entry point with the completion obligation riding on the token (`ResidentHole`), vs. repl's five parallel `resume_*` functions selected by an external `match` (§1.1, §1.3). harness's shape is the one that already replaced a real bug class (silently dropping a bind obligation) — a kernel should generalize *this* one. |
| Multi-hole support | **harness** | Repl is single-hole by construction (one `ask` at a time is the documented contract); harness supports N parked holes per session, resumable in any order. If the kernel is meant to also serve a future repl surface with more than one suspending effect, only harness's shape scales. |
| Suspension typing / schema backing | **harness, post-`a95eac61`** | Exhaustive-by-construction roster decode generated from `tidepool-protocol`, with a `Decode`-vs-`UnsupportedConstructor` failure split. Repl's one-member "roster" (`extract_ask_request`) is the same pre-migration pattern harness just retired — see §6 for a low-risk fix that doesn't wait on anything else in this doc. |
| Resume-rejection error structure | **repl** | `suspension_for`'s three-way `Err(None) / Err(Some(pending)) / Ok(..)` keeps "what's actually pending" as structured data all the way to the caller. Harness's equivalent (`SessionMismatch{detail: String}`) has the same information but flattens it into a display string (§1.5). |
| Abandoned-suspension liveness | **repl** | A real TTL reaper with an explicit, visible `Wedged` terminal state. Harness has no time-based cleanup for an orphaned hole at all — an unanswered suspension is invisible-but-permanent unless something explicitly cancels the node. This is a genuine gap worth carrying into the kernel design rather than something to silently drop because "harness never needed it before." |
| Typed error taxonomy breadth | **harness** | `RoutingMismatch`, `BorrowedRootOnBindingHole`, `ClassifyError::Decode` are all *new*, precise failure modes repl has no equivalent of, mostly because repl's single-effect, single-hole surface never produced them. Breadth here tracks harness's larger suspension surface, not necessarily better design taste — worth naming, not necessarily worth copying wholesale onto repl's simpler surface. |
| Busy-guard / admission policy | **repl, as a pattern** | `admit_run` is explicitly documented as *policy layered on top of* the shared registry, not the registry's own opinion (§1.2). This separation-of-concerns is exactly the shape a kernel-level "policy hook" should adopt: the kernel enforces nothing about whether a fresh run may proceed over a suspended session; each consumer supplies its own admission policy, same as today. |
| Checkpoint/durability | **harness, but incompletely** | Has a durable audit log the repl entirely lacks — but neither side actually reconstructs a live suspension from durable state today (§1.4). Not a place to copy harness's story uncritically; both are "in-memory only" in the sense that matters for a kernel's resume-availability contract. |

---

## 3. The proposed kernel seam

### 3.1 What is already shared and should not be re-litigated

`PersistentSession` (`tidepool-runtime/src/session/persistent.rs`) — the
machine + accumulated `DataConTable` + decl/value planes + scope tree — is
already the one substrate both `ResidentSession` (harness) and repl's
`Session` build on. `SessionRegistry`/`Slot`/`Checkout` (§1.2) is already
the one ownership mechanism. `GateDispatcher` (the shared per-turn
abort-checkpoint wrapper) is already shared. None of this needs a kernel —
it needs to stay exactly where it is.

### 3.2 What the kernel should own

A thin orchestration layer sitting directly on the machine-level suspend
primitive (either `PersistentSession`'s slot path or `ResidentSession`'s
parked path — see the Phase-6 fork in §5/open questions for why this
sentence has to stay conditional), providing:

1. **One obligation-carrying hole token**, generalizing `ResidentHole`
   (§1.1/1.3) rather than repl's five-tail split. A consumer that needs a
   fifth materialization policy in the future adds a variant to the token,
   not a sixth parallel resume function.
2. **One resume/abort entry point** per token, mirroring `ResidentSession::
   resume`/`abort` — never a second, externally-dispatched family.
3. **A shared error taxonomy** for the resume-rejection space, taking
   repl's *structural* three-way result (§1.5, §2) as the base shape and
   harness's *breadth* (`RoutingMismatch`, `Decode` vs `Unsupported
   Constructor`) as the additional variants layered on top — not
   flattening structured data into a `String` the way `SessionMismatch`
   currently does.
4. **A policy hook, not a policy**, for admission-while-suspended: the
   kernel's own checkout primitive permits a run over a suspended slot
   (today's registry behavior, needed for harness's multi-hole story); a
   consumer that wants repl's stricter "refuse until resumed/reset"
   contract supplies that as a wrapper, exactly as `SessionManager::
   admit_run` does today. The kernel should not have an opinion here.
5. **An abandonment-liveness contract**, generalizing repl's TTL reaper +
   `Wedged` terminal state so that harness's currently-nonexistent
   orphaned-hole cleanup becomes at least *possible* to opt into, without
   forcing every harness node to pay for it (a companion loop's answerer
   holes are meant to sit suspended indefinitely awaiting an operator; a
   TTL default there would be actively wrong). This should be exposed as
   an optional per-consumer policy, same shape as item 4.

### 3.3 What stays a policy of repl/harness respectively

- **repl**: the single-implicit-session shape (`SingleSlot`, no session
  name), the busy-guard (`admit_run`), the TTL reaper + `Wedged` handling,
  the block-runner's item classification (decl/stmt/meta) and its five
  materialization-policy tails *as consumers of the kernel's one token*,
  the `:reset`-folds-abort UX.
- **harness**: the `NodeTree`/`NodeState` lifecycle (`Thunk → Running →
  Suspended → Done/Cancelled`, `forcing.rs`), `pending_suspensions`'s
  node-scoped domain metadata (`ClassifiedSuspension`, `raw_request`,
  `suspend_table`/`suspend_asks`), the durable JSONL log, the whole
  `SuspensionRouting` orchestration match in `classify_hole` and its
  driver-side consumption (`service_askuser_hole`, `answer_dialog`,
  `service_outer_fanout`, …), `AnswerContract`/`Finalize`-row pinning,
  fork/fanout budgets, the one-session collapse's realm/scope machinery.

None of the domain-specific orchestration (what a `Finalize` hole *means*,
what a `:program` notebook repaint *is*) belongs in the kernel — only the
shape of "park on a typed hole, resolve externally, resume through one
entry point" that both currently reimplement independently.

---

## 4. The crate decision

### 4.1 The precedent, weighed honestly

The registry-capstone wave (`plans/registry-capstone.md`) already answered
this *exact* question once, for the ownership layer: promote into
`tidepool_runtime::session::registry` as a **new module** under the
existing session substrate, not a new crate — `repl`'s `manager.rs` and
harness's `registry.rs` became thin type-alias clients (§1.2). `Persistent
Session` itself is *also* already shared the same way, in the same
`tidepool_runtime::session` module. This would be the **third** consolidation
of this exact shape (repl/harness sharing a session-lifecycle mechanism)
landing in `tidepool_runtime::session` — precedent argues strongly for a
fourth module there, not a new crate.

### 4.2 The utility-attractor worry, and where it does and doesn't apply

The wave brief is right that `tidepool-runtime` is accumulating unrelated
responsibilities — `paths.rs` (config/cache resolution), `session::turn`
(Haskell turn-module templates), `session::supervisor` (`TurnSupervisor`),
`cache.rs`/`artifacts.rs` (the compile memo), `toolchain.rs`. A crate that
holds "config paths" *and* "compile caching" *and* "session lifecycle" is a
plausible dumping ground in the abstract.

But the kernel's marginal addition is **not** a new *kind* of
responsibility for this crate — it is a deeper version of a responsibility
(`session::*`) that already anchors two of the crate's existing top-level
concerns (`PersistentSession`, `registry`, `resident`, `turn`). The
utility-attractor risk is real *at the crate level* (should compile-caching
and session-lifecycle share a crate at all? — a fair question, but not
this doc's question and not one #22 asks), not at the level of "should the
session module get one more session-lifecycle piece."

### 4.3 A concrete, non-precedent argument against a new crate

`ResidentSession`'s encapsulation is deliberately tight in ways a new crate
would have to punch holes through: `HoleSeed` (`resident.rs:314-320`) is a
private enum; `ResidentHole::mint`/`seed` (`resident.rs:270-293`) are
non-`pub` methods; the module doc is explicit that `PlainHole`/`BindingHole`
have "no public constructor and no public field — the only way to obtain
one is a suspension surfaced by this session's own `run*`/`resume`
methods," and that this is what makes `resume` a "single, unconditional
entry point" rather than requiring an external "is this pending a bind"
flag (`resident.rs:220-235`). A kernel crate living *outside*
`tidepool-runtime` that needs to construct, inspect, or dispatch on these
tokens would need at least `pub(crate)` → `pub` promotions specifically to
serve it — which is a real, current-code cost of the new-crate option, not
a hypothetical one. Doing the equivalent work as a new module *inside*
`tidepool-runtime::session` needs no such promotion; `pub(super)`/module-
private visibility already reaches it.

### 4.4 Recommendation

**Extend `tidepool_runtime::session` with a new module** (a name like
`session::kernel` or `session::suspend` — bikeshed for the operator, not
decided here), following the registry-capstone playbook exactly: the
module owns the generalized token/resume/error-taxonomy shape (§3.2);
`tidepool-repl` and `tidepool-harness` become progressively thinner clients
of it, the same relationship `manager.rs`/`registry.rs` already have to
`tidepool_runtime::session::registry`.

This goes against the wave brief's "perhaps in own crate" framing, so it is
flagged as **Open Question 2** below rather than assumed — the operator may
have context (a planned crate split, a build-time/dependency-graph concern
this doc didn't surface) that changes the calculus. But the honest
argument, weighed against the one directly comparable precedent this
codebase has already executed, points at "another `tidepool-runtime::
session` module," not a new crate.

---

## 5. Migration order

This section is written twice — once per answer to **Open Question 1**
(§ below) — because the two orders diverge from the first step.

### 5.A If Phase 6 (one-session.md) proceeds first

1. Land Phase 6 as already scoped in `plans/one-session.md`: convert repl
   and one-shot eval onto the parked path (`Project`/`Render` park kinds
   already exist per Phase 0; the repl-side work is stowing `run_block`'s
   item-loop state, per `plans/unpark/feasibility-map.md`'s own mechanical
   assessment — that survey already found "no mechanism conflict"). Delete
   the slot path (`suspended_continuation`, the four slot run/resume
   families) per Phase 6's own scope. Oracle: the repl test suite,
   byte-identical behavior, per Phase 6's stated gate.
2. With both crates now on `ResidentSession`, extract the kernel module
   (§3.2/§4.4) as a **pure refactor**: pull the token/resume/error shape up
   out of `ResidentSession` into the new `session::kernel` module,
   `ResidentSession` becomes a thin client of it exactly as it is a thin
   client of `PersistentSession` today. No behavior change; oracle is both
   crates' existing suites, unmodified.
3. Migrate `tidepool-harness`'s `Harness`/`engine.rs` to consume the
   kernel's shared error taxonomy where it currently duplicates concepts
   (e.g. folding `HarnessError::SessionMismatch`'s stringly-typed detail
   back into the kernel's structured `WrongHole`-shaped variant, closing
   the gap §1.5/§2 identifies).
4. Migrate `tidepool-repl`'s `SessionManager` to consume the kernel's
   admission-policy hook (§3.2 item 4) instead of hand-rolling `admit_run`
   inline, and the abandonment-liveness hook (§3.2 item 5) instead of its
   own bespoke reaper — same behavior, less duplicated plumbing.
5. Each step lands green on both crates' full suites before the next
   starts; no step changes wire bytes, model-facing prompt text, or
   checkpoint shape.

### 5.B If the kernel is sequenced before/independent of Phase 6

1. Define the kernel's token/resume/error-taxonomy shape as a **trait
   seam** in the new module — e.g. something in the shape of `trait
   SuspendableSession { type Hole; type Answer; fn resume(&mut self, hole:
   Self::Hole, answer: Self::Answer) -> Result<Outcome, KernelError>; }` —
   generic enough that both `ResidentSession` (parked) and a slot-path
   wrapper (single-hole, one token variant) can implement it without either
   side changing its underlying suspend mechanism yet.
2. Implement the trait for `ResidentSession` first (harness is already
   closest to the target shape — this is a much smaller diff than repl's
   side).
3. Implement the trait for repl's slot-path `Session` via an adapter that
   presents the existing `PendingTail` five-way split as one token variant
   internally — **this adapter is explicitly throwaway**: it exists only to
   let repl consume the kernel's error taxonomy and admission-hook shape
   before Phase 6 lands, and gets deleted the moment Phase 6 converts repl
   onto `ResidentSession` directly (at which point repl uses the exact same
   trait impl harness does, no adapter needed).
4. Each crate migrates its own call sites onto the trait incrementally,
   both suites green at every commit.
5. When Phase 6 eventually lands, delete the slot-path adapter from step 3
   as part of that phase's own "slot path deleted last" step — this is
   already Phase 6's documented final step, just with one more throwaway
   file to remove.

**5.A is the recommended order** if the operator is willing to unblock
Phase 6's soak gate — it does strictly less total engineering (no
throwaway adapter, no abstracting over two suspend mechanisms) for the same
end state. 5.B exists because the wave brief's own framing ("perhaps in own
crate," treating #22 as independent of #20/#21) suggests the operator may
want the kernel decoupled from Phase 6's timeline; if so, 5.B is how to do
it without blocking on Phase 6's soak criteria.

---

## 6. Interaction with the suspension-schema plane (#20)

### 6.1 What the generated typed decode already contributes

`a95eac61` (#20 step 1) gives the kernel design two things for free on the
harness side: (1) a **composable roster** vocabulary (`RosterRequest`,
generated per-effect `FromCore` decode) that a kernel-level "typed hole"
abstraction should key its harness-side instantiation on directly, rather
than re-deriving its own notion of "what suspending effects exist"; and (2)
a precedent for the `Decode`-vs-`UnsupportedConstructor` error split that
§3.2's shared error taxonomy should adopt outright rather than reinvent.

### 6.2 A concrete, decoupled win for repl (do this regardless of §5's answer)

repl's `extract_ask_request` (§1.3, `tidepool-runtime/src/session/engine.rs:
1211-1235`) hand-matches `"AskWith" | "RunLLMTurnWith"` — structurally
identical to harness's pre-`a95eac61` roster, just with one member so it
has never been bitten by drift. `tidepool_protocol::effects::ask` already
exists and already generates `crate::generated::ask::AskReq` for the
harness (`engine.rs:467`, `RosterRequest::Ask`). There is nothing about
`#22`'s bigger questions blocking a small, independent fix: point repl's
`extract_ask_request` at the same generated `AskReq: FromCore` decode
instead of its hand-rolled constructor match. This is low-risk (repl has
exactly this one suspending effect, so the blast radius of getting the
decode wrong is immediately visible), decoupled from the Phase-6 question,
and closes the identical roster-drift hazard class on repl's side that
`a95eac61`'s commit message names as its whole motivation for harness. It
does not require the kernel module to exist first — it is a fine independent
task, but worth sequencing *before* or *alongside* the kernel work rather
than after, since it removes one more repl/harness asymmetry the kernel
would otherwise have to explain away.

### 6.3 Should #20 steps 2-3 land before or after kernel implementation?

**Recommendation: after**, and only loosely coupled even then. Steps 2-3
(`plans/harness-architecture-wave.md`'s "Held briefs") move the *Haskell
decl text* (`typed_request_agent_decls` et al.) onto the generated plane and
add schema support for polymorphic verbs (`fork @T`, `runLLMTurn @T`) —
this is model-facing prompt surface and genuinely novel design work (the
wave doc's own words: "the reason the decls were hand-written originally").
None of it changes the *Rust-side* orchestration shape (`SuspensionRouting`,
`ClassifiedSuspension`, the driver's match arms) that the kernel seam
(§3.2) actually generalizes — step 1 already delivered everything the
kernel needs from the schema plane on the decode side. Sequencing kernel
implementation ahead of steps 2-3 means the kernel is built against a
stable, already-migrated decode layer without waiting on a separately-risky,
prompt-text-changing, cache-affecting piece of work that has its own
operator-ping gate independent of #22.

The one coupling worth naming: if step 3's polymorphic-verb schema support
changes *how* a fork/fanout answer type is threaded through `Classified
Suspension`'s `ty`/`site` fields, the kernel's generalized token (§3.2 item
1) should be shaped to absorb that without a second migration — i.e. the
kernel's token design should not hard-code "one payload type per hole kind"
in a way that would need reworking once step 3 lands. This is a
design-time constraint on the kernel's token shape, not a sequencing
requirement.

---

## 7. North star: one orchestration surface, delegate-mounted sessions (operator direction, 2026-08-24)

Operator statement, recorded verbatim in intent: the MCP eval surface and
the harness should eventually unify into **one orchestration/collaboration
surface** — and when a harnessed model delegates (spins off a coding
agent), that agent should ideally receive a tidepool MCP tool whose evals
run in a **shared environment with shared decls** — the spawning session's
substrate, not a fresh isolated server.

What this adds to #22's requirements, stated as constraints on the kernel
seam rather than new work:

- **The kernel serves N frontends, not 2.** The survey in §1 compares repl
  and harness, but the mount list the seam must not preclude is: harness,
  repl, the local one-shot MCP server, and — the genuinely new one —
  **network-mounted delegate sessions**: an MCP endpoint served *from the
  harness process*, bound to a scope/realm, whose connection info is handed
  to a spawned agent so its `tidepool` MCP client evals against the shared
  machine instead of a private stdio server.
- **The sharing semantics already exist in-process.** Scope trees (PRD 21
  C2) are exactly "shared decls, isolated children": a mounted session's
  decl scope seeds from the spawning node's tip (parent decls callable),
  its binds shadow locally, siblings are invisible, retirement is scope
  exit. A delegate mount is an attached realm + scope reachable over a
  transport — new plumbing, not a new sharing mechanism.
- **The effect row is the per-mount capability policy.** A delegate-facing
  mount can carry a row scoped to its containment (e.g. `Fs`/`Exec` bounded
  to its worktree, no recursive `Subagent`) while the companion's own row
  stays suspending-only. Nothing about the kernel token/resume shape should
  assume all mounts share one row.
- **Bearing on Open Question 2 (crate vs module):** a kernel that must be
  mountable by an in-process driver, a second binary (repl), and a
  network-facing endpoint strengthens the original "own crate" lean the
  operator voiced — the §4.4 module recommendation was weighed before this
  requirement existed and should be re-weighed with it on the table. Not
  decided here; flagged so the review sees both arguments.

None of this is scheduled by this doc — it is the direction the seam must
not design away.

## Open questions (for the operator, not decided here)

1. **Does #22's kernel wait on Phase 6 (`plans/one-session.md`), or is it
   explicitly meant to be decoupled from it?** This is the single biggest
   unknown in this doc — it changes §5's entire migration order and
   materially changes how hard "the kernel" is to build (generalizing one
   already-converged mechanism vs. abstracting over two permanently
   different ones). Phase 6 is currently "parked behind a production-soak
   gate, not currently in flight" (`plans/README.md:120-124`) — is that gate
   still the operator's intent, or has the harness's recent production
   mileage (the companion, the wave's own live-round trigger for #20
   steps 2-3) changed the calculus for unblocking it?
2. **tidepool-runtime module vs. new crate (§4)** — this doc recommends the
   former, against the wave brief's "perhaps in own crate" framing.
   Confirm or override.
   **ANSWERED (operator, 2026-08-24): no strong opinion on crate vs module —
   the binding goals are (i) ONE shared suspension/parking mechanism, (ii)
   cleanly factored-out components, (iii) abstraction boundaries expressed
   through the type system/traits, not convention.** The packaging call is
   therefore an implementation-time judgment: pick whichever container makes
   the trait seams cleanest, and let §7's multi-frontend requirement tip it
   if the module form would force a frontend to reach through non-public
   internals.
3. **Should the kernel's abandonment-liveness hook (§3.2 item 5) become the
   harness's first-ever TTL/reaper mechanism, or is an unbounded-suspension
   node an intentional invariant of the harness's design (an answerer
   node is *meant* to wait indefinitely for an operator) that a generic
   hook would be wrong to default-enable?** §2 flags this as a real gap;
   this doc does not have enough context on the harness's operational
   history (has an orphaned hole ever actually been a problem in
   production?) to recommend a default.
4. **Should `HarnessError::SessionMismatch`'s current `String`-flattening of
   `CheckoutError::WrongHole`'s structured `{session,attempted,parked}`
   data (§1.5) be fixed as part of the kernel migration, or is that a
   free-standing small fix worth doing independently right now?** Similar
   in spirit to §6.2's repl fix — small, decoupled, and arguably shouldn't
   wait on the rest of this design.
