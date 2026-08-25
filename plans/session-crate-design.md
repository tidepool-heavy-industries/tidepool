# Session-crate promotion + multi-frontend mounting — design

Status: DESIGN ONLY, for operator review. No code in this change. Written
against tip `1502d0d9` (post `root.schema-plane-decls` merge, post the
2026-08-24 `tidepool-toolchain` carveout `a379a863`, post the `session::kernel`
landing this week). Line numbers below are read-aids as of this commit, not
authoritative — relocate every claim by symbol name if the tree has moved.

This is the operator-approved "option B" follow-up named when the toolchain
carveout landed: `tidepool-runtime/CLAUDE.md`'s charter already flags the
tension — *"still not one cohesive concern [after the toolchain split], but
splitting the session substrate out further is a separate, sequenced structure
lane, not done here."* This doc is that lane, with one new input the prior
carveout didn't have to consider: the operator's stated north star (recorded
in `plans/resident-session-kernel-design.md` §7, verbatim below) is **N
concurrent frontends sharing one resident session** — not just "move the
files," but "move the files in a shape that doesn't have to be redesigned the
day multi-mount lands."

---

## 0. What "multi-frontend" means, stated once

Operator statement, recorded verbatim in `plans/resident-session-kernel-design.md`
§7 (2026-08-24): the MCP eval surface and the harness should eventually unify
into **one orchestration/collaboration surface**, and when a harnessed model
delegates (spins off a coding agent), that agent should ideally receive a
tidepool MCP tool whose evals run in a **shared environment with shared
decls** — the spawning session's substrate, not a fresh isolated server.

Concretely, the frontend list this design must not preclude (§7's own list,
plus the two already-shipping ones it's implicitly compared against):
`tidepool-repl` (one process, `SingleSlot`, human/Claude via MCP stdio),
`tidepool-harness` (one process, keyed `SessionRegistry`, N tree nodes),
the one-shot MCP eval server (`tidepool`, no residency at all — a fresh
`compile_and_run` per call), and — the genuinely new one — a
**network-mounted delegate session**: an MCP endpoint served *from the
harness process*, bound to a scope/realm, handed to a spawned
`tidepool-agent` coding backend so its own `tidepool` MCP client evals
against the shared machine instead of a private stdio server.

Nothing in §7 is scheduled or implemented today (confirmed by grep: no
`network-mounted`, `delegate session`, or "MCP endpoint served from the
harness" text exists outside that one plans doc; `tidepool-agent`'s seam
(`tidepool-agent/CLAUDE.md`) has no session-mount concept yet). This doc's
job is narrower than designing that endpoint — it is to promote the crate in
a shape that endpoint can be built against later without another structural
move.

---

## 1. What moves

### 1.1 The nine modules, and their real size

`tidepool-runtime/src/session/` (`mod.rs` declares all nine as `pub mod`,
lines 21-28):

| Module | Lines | What it is |
|---|---|---|
| `mod.rs` | 856 | `SessionLib` (`mod.rs:162`) — decl-plane source-text accumulation, its own module doc's subject |
| `kernel.rs` | 286 | `SuspendableSession`/`Aged`/`admit_checkout` (#22's landed kernel — see §1.4) |
| `registry.rs` | 1066 | `SessionRegistry`/`Slot`/`Checkout`/`CheckoutError`/`SingleSlot` (the ONE session-ownership mechanism, root `CLAUDE.md` Mechanism Index) |
| `persistent.rs` | 1171 | `PersistentSession` (`persistent.rs:76`) — machine + accumulated `DataConTable` + decl/value planes + scope tree |
| `resident.rs` | 2201 | `ResidentSession`/`ResidentHole`/`RootCustody` — the parked-path, multi-hole resident machine `tidepool-harness` drives |
| `engine.rs` | 1654 | `SessionEngine`/`GateDispatcher`/`TurnOutcome` — the oneshot, channel-driven stow-engine `tidepool-repl`'s slot path and the one-shot MCP server drive |
| `turn.rs` | 2176 | `compile_session_turn`/`run_turn`/`CompiledTurn` — the turn-module compile pipeline (its own `ExtractCmd` invocation, independent of the facade's `compile_haskell`) |
| `render.rs` | 1529 | `DeclLog`/`DeclTurn`/`RenderedModule`/`ExportItem` — SESSION-TURN rendering (gen-versioned module text), **not** the top-level `tidepool-runtime/src/render.rs` (see §1.2's naming collision) |
| `supervisor.rs` | 85 | `TurnSupervisor` (`supervisor.rs:50`) — the turn-abort watchdog mechanism, root `CLAUDE.md` Mechanism Index entry "turn thread supervision" |

11,024 lines total (`wc -l`), against `tidepool-runtime/src/{lib.rs,render.rs,failclass.rs}` at roughly 1,900 lines combined post-toolchain-carveout (`lib.rs` 99 lines shorter than pre-carveout per `a379a863`'s diffstat, plus the untouched `render.rs`/`failclass.rs`). The session substrate is already the large majority of this crate by line count — the charter's own "still not one cohesive concern" is, concretely, an ~11k/~13k split.

### 1.2 What stays — and a naming collision worth flagging now

**Stays in `tidepool-runtime`:**

- The facade: `compile_haskell`/`compile_haskell_salted`/
  `compile_and_run`/`compile_and_run_with_nursery_size`/
  `compile_and_run_cancellable`/`compile_and_run_suspendable`/
  `resume_suspended_turn`/`compile_and_run_pure`/`compile_and_run_pure_salted`
  (`lib.rs:91-404`), plus `CompileResult`/`RuntimeError`/`SuspendableRun`/
  `ResumedRun` and the two sizing constants `EVAL_STACK_SIZE` (`lib.rs:153`)
  and `DEFAULT_NURSERY_SIZE` (`lib.rs:146`).
- `tidepool-runtime/src/render.rs` — `EvalResult`/`value_to_json`
  (top-level, re-exported at `lib.rs:49`). The toolchain carveout's own
  commit (`a379a863`) already ruled on this exact file once: *"Deviation
  from spec (parent-confirmed): src/render.rs (EvalResult/value_to_json)
  stays in tidepool-runtime — it's eval-result JSON rendering, unrelated to
  the toolchain."* This doc does not re-litigate that ruling.
- `failclass.rs`'s `classify`/`classify_session` (dispatch over
  `RuntimeError`/`SessionError`, which is why they couldn't move to
  `tidepool-toolchain` either — same reasoning applies one layer up: they
  can't move to a session crate without RuntimeError following them, and
  RuntimeError has independent reasons to stay, below).
- The `tidepool_toolchain` re-export shims (`paths`, `toolchain`, `cache`,
  `artifacts`, `diag`, `timing`, `CompileError` — `lib.rs:27-28`).

**Naming collision to resolve at implementation time, not decided here:**
`tidepool-runtime/src/render.rs` (facade, `EvalResult`) and
`tidepool-runtime/src/session/render.rs` (session-turn module rendering,
`DeclLog`/`RenderedModule`) are two same-named, unrelated files today,
disambiguated only by the `session::` path prefix. Promoting `session/` into
its own crate means the facade's `render.rs` stays `tidepool_runtime::render`
and the session one becomes `tidepool_session::render` — the collision
resolves itself for free at the crate boundary (two crates, two `render`
modules, no clash), but is worth naming so nobody "fixes" it by renaming one
file mid-migration under the impression it's a pre-existing bug.

### 1.3 The dependency edges — and why the toolchain carveout's "zero call-site diff" trick does NOT transfer

The toolchain carveout achieved zero call-site changes because the extracted
material (`paths`/`toolchain`/`cache`/`artifacts`/`diag`/`timing`) sat
*below* `tidepool-runtime` in the dependency graph — nothing in it depended
on facade-level types, so `tidepool-runtime` could keep re-exporting the
moved modules as thin shims (`pub use tidepool_toolchain::{...}`) and every
existing `tidepool_runtime::toolchain::X` path kept resolving.

**The session module's dependency edges point the other way.** Grepped
directly against `session/*.rs`:

- `resident.rs:74` — `use crate::render::EvalResult;` (the FACADE's
  `render.rs`, not session's own).
- `resident.rs:76` — `use crate::{JitError, RuntimeError, EVAL_STACK_SIZE};`
  — `RuntimeError` is defined in `lib.rs:64`, not re-exported from anywhere
  lower.
- `turn.rs:33` — `use crate::{extract_module_name, timing, CompileError};`
  — `extract_module_name` is `pub(crate)` at `lib.rs:29`
  (`pub(crate) use tidepool_toolchain::{extract_module_name,
  extract_spawn_error};`) — visible today only because `turn.rs` is IN the
  same crate. A promoted `tidepool-session` crate cannot reach a `pub(crate)`
  item across a crate boundary at all.
- `mod.rs:109` — `Toolchain(#[from] crate::toolchain::ToolchainError)`,
  `mod.rs:132` — `crate::CompileError` — both shim re-exports, reachable
  either via `tidepool_runtime::{toolchain, CompileError}` OR directly via
  `tidepool_toolchain::{toolchain, CompileError}` (the shim's own source).
- `turn.rs:478`, `mod.rs:712` — `crate::paths::apply_build_products_dir`
  (shim; same either-path property as above).
- `turn.rs:605,972,1136`, `mod.rs:742,756,758`, `engine.rs:145,221,1102` —
  `crate::diag::{parse_diag_report, render_diagnostics, RenderOpts,
  ExtractDiag}` (shim; same either-path property).
- `resident.rs:2037` (test-only) — `crate::DEFAULT_NURSERY_SIZE`.

Two different shapes here, and they resolve differently:

1. **The toolchain-shim edges** (`paths`, `diag`, `toolchain`,
   `CompileError`) are false dependencies on `tidepool-runtime` specifically
   — every one of them is a re-export of `tidepool_toolchain`, a crate that
   already sits *below* both `tidepool-runtime` and any future
   `tidepool-session`. A promoted session crate should depend on
   `tidepool-toolchain` DIRECTLY for these (`tidepool_toolchain::{paths,
   diag, toolchain, CompileError}`), the same way `tidepool-runtime` itself
   does post-carveout — no detour through the facade crate, no new shim
   needed. `extract_module_name` needs its `pub(crate)` widened to `pub` in
   `tidepool-toolchain` (mirroring exactly what the toolchain carveout
   already did for `CompileInvocation`/`CacheStrategy`/`compile_invocation`
   per `a379a863`'s own commit message) — a narrow, precedented visibility
   change, not a design question.
2. **The facade-type edges** (`EvalResult`, `RuntimeError`, `JitError`
   re-export, `EVAL_STACK_SIZE`, `DEFAULT_NURSERY_SIZE`) are real: a
   resident turn's completed outcome IS an `EvalResult`
   (`ResidentOutcome::Completed{result: EvalResult, ..}`, `resident.rs:339`),
   and its failure mode IS a `RuntimeError` (`ResidentError::Run(RuntimeError)`,
   `resident.rs:396`). These types cannot be duplicated (the one-mechanism
   rule) and cannot be trivially relocated without relitigating the toolchain
   carveout's own explicit ruling that `EvalResult` "stays in
   tidepool-runtime... unrelated to the toolchain" (§1.2) — moving it now,
   to serve a crate split it didn't anticipate, would be exactly the kind of
   drive-by re-derivation the doc-history rule warns against. `RuntimeError`
   has an independent reason to stay put too: `tidepool-toolchain/CLAUDE.md`'s
   own charter states outright that it "must not be visible" there, since it
   dispatches over `RuntimeError`/`SessionError` — types owned one layer up.

**Conclusion: `tidepool-session` depends on `tidepool-runtime` (the
slimmed-down facade), not the other way around.** This is the opposite
direction from the toolchain carveout, and it has one concrete, honest
consequence: **`tidepool-runtime` cannot re-export `session` as a thin shim**
— doing so would require `tidepool-runtime → tidepool-session →
tidepool-runtime`, a cycle. Every existing `use tidepool_runtime::session::*`
call site (§1.4 below enumerates them) needs its import path changed to
`tidepool_session::*` and its crate's `Cargo.toml` needs the new dependency
added. This is bounded and mechanical (grep + replace across a handful of
crates), but it is real churn this doc should not undersell by analogy to
the toolchain carveout's zero-diff property. See §4 for the migration
sequencing this implies.

### 1.4 Who calls in today (the promoted crate's real consumer set)

```
grep -rl "tidepool-runtime" --include=Cargo.toml .
```
lists `tidepool-handlers`, `tidepool` (composition root), `tidepool-web`,
`tidepool-testing`, `tidepool-harness`, `tidepool-repl`, `tidepool-mcp` as
depending on `tidepool-runtime` today. Of these, the `session::` submodule
specifically is consumed by (per `tidepool-harness/CLAUDE.md` and
`tidepool-repl/CLAUDE.md`, both read for this doc):

- **`tidepool-harness`**: `registry.rs` type-aliases
  `tidepool_runtime::session::registry::{SessionRegistry<M>, Checkout<'_,M>,
  CheckoutError}` at `H = HoleId`; `Harness` instantiates
  `NodeTree<ResidentSession<BoxedStack, CapturedOutput>>` — a direct,
  load-bearing dependency on `session::resident::ResidentSession` and
  `session::registry`.
- **`tidepool-repl`**: `manager.rs`'s `SessionManager` wraps
  `tidepool_runtime::session::registry::SingleSlot<M, ContinuationId>`;
  `session.rs` drives `tidepool_runtime::session::PersistentSession`'s slot
  path directly (the pre-#22-kernel mechanism, still live per
  `resident-session-kernel-design.md`'s TL;DR — Phase 6 is parked, not
  landed).
- **`tidepool-mcp`**: consumes `SessionLib` (`mod.rs`) for its decl-plane
  accumulation surface (referenced by `tidepool-runtime/CLAUDE.md`'s
  charter line "the declaration-accumulation lane (`session::SessionLib`)").
- **`tidepool` (composition root)**: the one-shot MCP eval server drives
  `session::engine::SessionEngine` per `tidepool-runtime/CLAUDE.md`'s
  charter ("`turn supervision`... and the `classify`/`classify_session`
  half of failure classification").

Four consumer crates, not a long tail — the bounded-migration claim in §1.3
rests on this count staying small.

---

## 2. The multi-mount question

### 2.1 What already exists to build on

`SessionRegistry<M, H>` (`registry.rs:181-408`) is **already** the keyed,
N-session, N-hole-per-session primitive: `Idle(M) | Running{holes} |
Suspended{machine, holes} | Wedged{since}` (`registry.rs:107-130`), one
`checkout_run`/`checkout_resume`/`checkout_child` per transition, epoch-guarded
against a stale checkout resurrecting a removed/replaced entry
(`registry.rs:41-52`, `restore_suspended`/`mark_wedged`'s epoch compare). It
is generic over BOTH the machine handle `M` and the hole-identity type `H`
(module doc, `registry.rs:6-16`) specifically so a keyed multi-hole consumer
(`tidepool-harness`'s `HoleId`) and a single-hole consumer
(`tidepool-repl`'s `ContinuationId` via `SingleSlot<M,H>`, `registry.rs:659-777`)
share one mechanism without unifying those two hole-identity types.

`admit_checkout` (`kernel.rs:193-207`) is the ADMISSION-POLICY HOOK: given a
checkout and a caller-supplied `impl FnOnce(&[H]) -> bool`, it either returns
the checkout untouched (admitted) or restores it exactly as found and returns
the pre-checkout hole set (refused) — repl's `admit_run` busy-guard is
already this shape (`tidepool-repl/CLAUDE.md`'s "busy-guard policy" section),
generalized so a future consumer needn't re-derive the checkout-then-restore-
if-refused dance.

`SuspendableSession` (`kernel.rs:138-175`) is the ONE resume/abort entry
point per token — `type Hole`/`Answer`/`Context`/`Outcome`/`Error` associated
types, `fn resume(&mut self, hole, answer, cx) -> Result<Outcome, Error>` and
`fn abort(...)` — deliberately NOT a kernel-defined hole enum (§kernel.rs's
own doc, "Why `Hole` is an opaque associated type"), so a future
materialization policy never forces a second migration.

`Aged<T>` (`kernel.rs:60-108`) is the abandonment-liveness primitive: a
value plus its mint `Instant`, `age()`/`touch()`/`since()` — the kernel
drives no timer and reclaims nothing on its own (per the operator's OQ3
answer, `resident-session-kernel-design.md`'s Open Questions: "hook, no
default... indefinite park remains the harness's policy").

**What this gives multi-mount for free:** the registry's per-session hole
SET already means "one session, several outstanding suspensions, each
resumable independently" is a solved problem — `tidepool-harness`'s own
production case already does exactly this for concurrently-driven, attached
answerer realms sharing one machine (`tidepool-harness/CLAUDE.md`'s
"MULTI-HOLE (one-session plan, Phase 2)" section). What it does NOT give for
free — because nothing in the registry, the kernel, or `ResidentSession`
today has any notion of it — is a second axis: **N *frontends*, each
possibly issuing its OWN top-level runs against the SAME session
concurrently**, as opposed to today's shape where exactly one caller (the
harness driver's own turn loop, or the repl's single MCP client) is ever the
one issuing `checkout_run`/`checkout_resume` calls against a given session
id at a time.

### 2.2 What scope trees already give the sharing semantics multi-mount needs

`tidepool-harness/CLAUDE.md`'s "Scope trees" section (PRD 21 lane C2,
already landed) is the answer to "what does 'shared decls, isolated
children' concretely mean here" — a mounted delegate session does not need
a new sharing primitive, because this one already exists:

- A `ScopeId` (`tidepool_codegen::scope`) is minted per agent session
  (`with_session(sid, |s| s.mint_scope(parent))`), seeding the child's decl
  tip from its PARENT's at mint time — "a sibling that defines in between
  cannot leak in."
- A scoped turn compiles against `session_import_module_in(scope)` (its own
  decl tip, which re-exports its parent's chain) and
  `current_val_modules_in(scope)` (its own frame plus every ancestor's,
  nearest shadowing) — "parent declarations callable in every child,"
  "a sibling's names are not even nameable," "nothing ever walks downward."
- A `RealmId` is the HEAP-side lifetime counterpart (parked frames,
  outstanding `ValueHandle`s) — one node carries both, retiring one step
  (`Harness::terminate_node` → `exit_agent_session`, `close_realm` then
  `retire_scope`, that order load-bearing per the sole-ownership rule).

A network-mounted delegate session is, in this vocabulary, an attached
realm+scope pair reachable over a transport instead of only from the
in-process driver loop — "new plumbing, not a new sharing mechanism," per
`resident-session-kernel-design.md` §7's own framing, which this survey
confirms rather than merely repeats: `set_realm`/`set_scope`
(`resident.rs:650-672`) are already applied at the ONE site every
run/resume/child path goes through (`Harness::run_checked_out`,
`tidepool-harness/CLAUDE.md`'s Machine lifecycle section) — a mount seam
does not need a second application site, only a second CALLER of the
existing one.

### 2.3 The concurrency gap named precisely

Here is the concrete gap §2.1 flagged. `SessionRegistry::checkout_run`
(`registry.rs:274-298`) takes the map's mutex, inspects the slot, and — if
`Idle` or `Suspended` — moves the machine out into a `Checkout`, releasing
the mutex before the (blocking) turn runs. This is correct and sufficient
for TODAY's shape, where exactly one call site per session ever calls
`checkout_run`/`checkout_resume` (harness's own turn loop; repl's own RPC
handler) — a second concurrent caller against the SAME session id simply
gets `CheckoutError::Running` (`registry.rs:73-75`) and, in every consumer
today, treats that as "busy, try later" or an outright refusal.

**A second frontend attached to the same session is, by definition, a second
concurrent caller.** If an interactive Claude via the repl and a resident
harnessed companion are BOTH mounted on one session, and both attempt a turn
at the same wall-clock moment, one of them receives `CheckoutError::Running`
today — which is CORRECT (the registry's single-writer invariant, see §2.5,
must hold), but is not yet a designed EXPERIENCE: nobody has decided what a
frontend does with that refusal (queue silently? surface it to the human?
retry with backoff? are frontends ordered by priority?), because no consumer
today has needed to ask.

### 2.4 Three mounting-model options, with invariants

**Invariants every option below must hold** (derived from §2.1-2.2 plus the
registry's own documented contract, `registry.rs:26-39` and
`tidepool-harness/CLAUDE.md`'s "Machine lifecycle"):

- **I1 — one writer at a time.** Exactly one `Checkout` may be outstanding
  per session id at any instant (the registry's `Idle|Suspended → Running`
  transition already enforces this structurally — no design choice below
  may weaken it).
- **I2 — the machine is the ground truth for its own hole set.** A restore
  always carries the session's OWN reported holes (`registry.rs:522-524`'s
  doc), never a caller's guess — this must hold regardless of which frontend
  is doing the restoring.
- **I3 — decl-plane visibility follows scope, not frontend identity.** What
  a turn from frontend A can see (parent decls, sibling isolation) is
  governed by the SCOPE it compiles in (§2.2), never by which frontend
  issued the turn. Two frontends attached to sibling scopes must be as
  mutually invisible as two sibling harness nodes are today.
- **I4 — a suspension routes back to the frontend that asked, not to
  whichever frontend happens to be listening.** `Harness::pending_suspensions`
  already keys by `(SessionId, HoleId)` (`tidepool-harness/CLAUDE.md`'s
  "Suspension metadata" section) specifically because JIT continuation ids
  are minted per-machine and could collide across sessions — a
  multi-frontend design must extend this key (or an equivalent) to also
  carry frontend/mount identity, so a resume answer submitted by frontend B
  can never be misdelivered to a hole frontend A is waiting on, and vice
  versa.

**Option 1 — per-frontend leases (a new admission layer above the registry).**
Each frontend registers a lease token when it mounts; `checkout_run`/
`checkout_resume` gain a caller-supplied lease check via `admit_checkout`'s
existing hook shape (§2.1) — a lease-holding frontend's checkout is admitted,
a non-holder's is refused with a typed "another frontend holds the lease"
error (extending `CheckoutError`, not inventing a parallel taxonomy — the
kernel design's #22 §3.2 item 3 already commits to this: repl's structural
three-way error shape as the base, harness's breadth layered on top, never
flattened to a `String`). Mirrors `SessionManager::admit_run`'s existing
"policy layered on top of the shared registry, not the registry's own
opinion" pattern (`tidepool-repl/CLAUDE.md`) — a lease is exactly a
richer instance of the same hook, not a new mechanism. Simplest to build
(reuses `admit_checkout` verbatim); its cost is that a frontend without the
lease is simply locked out for the lease's duration — no fairness, no
interleaving, whichever frontend mounted-and-leased first monopolizes the
session until it releases.

**Option 2 — turn-admission queueing (a FIFO in front of `checkout_run`).**
Every frontend's turn request enters one queue per session; a dispatcher
drains it one `checkout_run` at a time, handing the result back to whichever
frontend's request it was. This is the compile-daemon's own precedent
(§3 below) applied one layer up: "one worker, a FIFO request queue" is
`plans/compile-daemon-design.md` §5.4's own operator-decided answer to an
almost identical single-mutable-resource-many-callers shape (GHC's session
there; the JIT machine's registry slot here). Guarantees I1 for free (only
the dispatcher ever calls `checkout_run`) and gives every frontend a fair
shot without a lease's "monopolize until release" property, at the cost of
a new stateful component (the queue + dispatcher) that does not exist today
and has its own crash/ordering semantics to design (what happens to a
queued-but-not-yet-dispatched request if its frontend disconnects?).

**Option 3 — fork-per-frontend-with-shared-parent (scope-tree native).**
Each frontend gets its OWN child scope+realm (§2.2's existing mechanism,
literally the mint-scope-per-agent-session pattern already used for fork
children) under one shared parent scope holding the common decls. A
frontend's own turns run against ITS scope, never contending for the
session-level `checkout_run` slot with a sibling frontend's turns AT ALL
(each child scope's turns still checkout the ONE underlying `Slot` — I1 is
unchanged, only the DECL/BIND visibility is per-scope) — this does not
remove the single-writer contention, it only means two frontends' turns are
structurally as isolated from each other as two harness fork children
already are (§2.2's I3), which may be what "concurrent frontends" actually
wants rather than genuinely simultaneous machine access (which I1 forbids
regardless of mounting model — the JIT machine itself has no concurrent-
access story on any option here, exactly as GHC's own session doesn't in the
compile-daemon's §5.4 finding). The turn-ADMISSION question (what happens
when frontend A's turn and frontend B's turn are both ready at the same
instant) is IDENTICAL to Option 1 or 2's under this framing — Option 3 is
about scope structure, not an alternative answer to admission, so it
composes with either of the other two rather than replacing them.

**This doc does not pick one.** Option 3's scope structure is close to
"assumed" by §2.2's already-landed mechanism (a mounted frontend without its
own scope would violate I3 the moment two frontends define same-named
decls), so it is likely load-bearing regardless of the admission answer;
Options 1 vs. 2 are the genuine fork, flagged as **Open Question 2** below.

### 2.5 What happens when a mounted frontend's turn wedges

`Slot::Wedged{since}` (`registry.rs:127-130`) is already the answer for a
turn that never gives the machine back — a TERMINAL placeholder, visible to
EVERY future caller (not just the original one — this was the pre-promotion
design's own bug, fixed by the registry consolidation per
`tidepool-repl/CLAUDE.md`'s "Wedged is a real, visible registry slot" note),
refusing every checkout with `CheckoutError::Terminal` until an explicit
`remove`/reinstall reclaims it.

**What is NOT yet answered: which mounted frontend gets to decide the
session is wedged, and what happens to every OTHER frontend's outstanding
work when it does.** Today, `mark_wedged` is called by the ONE caller that
had the machine checked out (repl's `SessionManager::mark_wedged`, or
harness's `run_checked_out`/`terminate_node` path) — there is exactly one
plausible caller because there is exactly one frontend. With N frontends, a
wedge caused by frontend A's turn (A's own compile/run hung past its abort
grace) becomes a Wedged session for frontends B and C too — every one of
their in-flight or future turns against that session now refuses with
`CheckoutError::Terminal`, with no way to distinguish "wedged by MY OWN turn"
from "wedged by someone else's turn I had nothing to do with." Per I4, this
needs the same session-plus-frontend-identity carrying that suspension
routing needs: at minimum, `Slot::Wedged{since}` (or a wrapping type one
layer up) should carry WHICH frontend's checkout wedged it, so an
uninvolved frontend's error message can say so rather than reading as its
own fault. **This is a gap, not a design — flagged as part of Open Question
2, since its answer depends on which admission model (§2.4) is chosen: a
lease holder wedging is a different story (the lease itself is now stuck)
than a queued request wedging (the queue's next entry is simply unblocked
once the wedge is reclaimed).**

### 2.6 What a frontend identity is

Not decided here (Open Question 3), but the shape is constrained by what
already exists: `tidepool-harness`'s `pending_suspensions` keys on
`(SessionId, HoleId)` and `NodeConvo`'s per-node bookkeeping already
distinguishes "which node" independent of "which session" — a frontend
identity is structurally closer to a NEW top-level key alongside `SessionId`
(mirroring how `HoleId` is already scoped per-session, not global) than to
overloading `NodeId` (which is a harness-internal tree-position concept that
a repl-mounted or network-mounted frontend has no reason to have). Whether
it is a bare string, a typed newtype, or reuses an existing identity
(a repl process's own MCP client connection id; a harness node's `NodeId`
when the frontend IS a harness node) is exactly the kind of "local, trivial"
ambiguity the Dev Agent Protocol says to resolve simply when the answer is
local — but "which existing identity, if any, a mount reuses" touches
`tidepool-harness`'s tree vocabulary and `tidepool-mcp`'s connection
vocabulary both, so it is flagged for the operator rather than assumed.

---

## 3. Network-mount precedent — what transfers from the compile daemon, what doesn't

`plans/compile-daemon-design.md` (decision-complete, Phase 0 shipped, Phase 1
scripts-wired-but-off-by-default per that doc's own status) is the house
precedent for "one resident process, many clients" — worth measuring a
session/repl mount against point by point rather than assuming it transfers
wholesale.

**What transfers directly:**

- **UDS, one request per connection.** §5.3's reasoning — same-host,
  same-user, no reason for a network-bindable service — applies identically
  to a session mount: nothing in §7's north star statement describes a
  cross-host use case, and `tidepool-agent`'s containment boundary
  (`tidepool-agent/CLAUDE.md`) already assumes same-host spawning.
- **EOF-as-crash-signal.** §5.3's "a broken pipe or unexpected EOF mid-
  response IS the daemon-crashed-mid-request signal, with no separate
  heartbeat protocol to design" is exactly the detection primitive a
  session mount needs too — no new liveness mechanism to invent.
- **Client fallback is not a special case.** §5.2/§5.5's shape — the
  daemon launcher is tried first, falling back to the existing path on any
  connect/timeout failure, so "no daemon" stays the untouched default —
  is the right shape for a delegate session mount too: a spawned
  `tidepool-agent` coding backend whose mount connection fails should fall
  back to its OWN private stdio `tidepool` server (today's shipping
  behavior), never hang waiting for a mount that isn't there.
- **The length-prefixed wire-framing correction (Decisions item 6).** The
  daemon's own design initially specified JSON-over-socket and was
  corrected to length-prefixed frames specifically because
  `tidepool-extract-cmd` is a zero-dependency std-only leaf. A session
  mount's transport constraint is different in KIND (its client is a full
  `tidepool` MCP server, already carrying `serde_json`/MCP's own framing —
  see below) but the underlying lesson — check what the actual endpoints
  can afford to depend on before picking a wire format — transfers as a
  METHOD, not as "reuse length-prefixed frames here too."

**What does NOT transfer — the two real differences:**

- **Sessions are stateful across connections; compiles are not.** The whole
  of §2's Session-scope-isolation design (§2.1-2.4 of the daemon doc) exists
  to guarantee that ONE daemon process serving MANY independent, mutually
  ISOLATING compile requests never leaks state between them — `load'`
  wiping the whole HPT every cycle, the `GutsMemo` sanitize-after-cycle fix
  (§7 deviation 1). A session mount inverts this goal entirely: the whole
  POINT is that N connections see the SAME machine's SAME accumulated
  state, deliberately, per §7's "shared environment with shared decls." Any
  isolation the mount needs (§2.2/§2.4's scope boundaries) is isolation
  BETWEEN sibling frontends' own sub-scopes, not isolation of the session
  from itself across requests — a categorically different property than
  anything §2 of the daemon doc builds.
- **A request is a TURN, not an idempotent compile.** The daemon's
  request/response shape (§5.3: one argv in, one `{exit_code, stdout,
  stderr}` out, connection closed) is stateless and safely retryable — a
  failed/timed-out request can simply be resent, because a compile has no
  side effect on the daemon beyond its own cache. A session turn is neither:
  it can SUSPEND (returning a hole, not a terminal result — §2 of this doc
  entirely) and it MUTATES the shared machine (a bind lands in the value
  plane, a decl lands in the decl plane) — resending a "failed" turn request
  after a connection drop risks a double-apply the compile daemon's design
  never has to consider, because nothing about a compile is stateful beyond
  its own memoization. A session mount's protocol therefore needs an
  idempotency or at-most-once story the daemon's request/response framing
  does not — likely closer to the existing `Checkout`/`CheckoutReceipt`
  discipline (an outstanding turn is a live, trackable obligation, not a
  fire-and-forget RPC) than to the daemon's connection-closes-when-done
  simplicity. This is real, unsolved design work belonging to whichever
  lane eventually builds the mount endpoint, not something this promotion
  needs to answer — but it is the reason "just reuse the daemon's protocol"
  is the wrong takeaway from §3's otherwise-real precedent.

---

## 4. Migration

### 4.1 Crate name

**Recommended: `tidepool-session`.**

Considered against the glossary's composition-over-coinage rule
(`docs/GLOSSARY.md` — bare "session" in prose must be qualified as **agent
session** or **machine session**; that qualification rule governs PROSE
inside docs, not a crate identifier, the same way `tidepool-harness` and
`tidepool-worktree` are themselves bare domain nouns used as identifiers,
not prose):

1. **`tidepool-session`** (recommended) — matches the module's own existing
   name (`session/`) and follows the established crate-per-domain-noun
   convention identically to every sibling crate (`tidepool-worktree`,
   `tidepool-harness`, `tidepool-toolchain`, `tidepool-repl`). No new term
   is coined; the promotion is a pure move of an already-named module to
   crate scope, which is exactly the case composition-over-coinage argues
   FOR the plainest available name. The crate's own `CLAUDE.md` should
   still use the glossary's qualified prose forms ("machine session" for
   `ResidentSession`/`PersistentSession` state, "agent session" for the
   turn-level interaction) — the identifier being bare does not exempt the
   doc prose inside it.
2. **`tidepool-session-kernel`** — rejected. This names the crate after
   `kernel.rs`, which is ONE of the nine modules moving (286 of 11,024
   lines) — the #22 kernel is the newest and most interesting piece, but
   the crate is not "the kernel plus incidental baggage," it's the whole
   session substrate the kernel sits inside. Naming the container after its
   most recent addition is a category error the composition rule flags
   directly: describe the whole, not the newest part.
3. **`tidepool-resident`** — rejected. "Resident" already has a narrower,
   established meaning one level down: `ResidentSession` (`resident.rs`)
   is specifically the PARKED-path, multi-hole machine, contrasted in its
   own module doc with `SessionEngine`'s oneshot "stow engine," which drops
   its machine after one turn and is explicitly NOT resident in that sense
   — yet `engine.rs` is equally part of what's moving. Using "resident" for
   the crate name would collide with the term's already-fixed narrower
   meaning inside the very code it names, exactly the kind of same-term-two-
   granularities ambiguity the glossary rule exists to prevent.

### 4.2 Shim policy — does not mirror the toolchain carveout, per §1.3

Because `tidepool-session` depends on `tidepool-runtime` rather than the
reverse (§1.3), `tidepool-runtime` cannot re-export `session` as a
zero-diff shim module the way it re-exports `paths`/`toolchain`/`cache`/etc.
today. The precedent's SPIRIT still applies — minimize churn, make the
compile error at every affected call site obvious and mechanical — but its
LETTER (a `pub use` shim needing no downstream change) is structurally
unavailable here. Concretely, for each of the four consumer crates named in
§1.4:

- `Cargo.toml` gains a `tidepool-session.workspace = true` dependency
  (alongside the existing `tidepool-runtime` one, which those crates keep
  needing for the facade + `EvalResult`/`RuntimeError`).
- Every `use tidepool_runtime::session::...` / `tidepool_runtime::session::X`
  path becomes `use tidepool_session::...` / `tidepool_session::X` — a
  mechanical, grep-verifiable rename (`rg 'tidepool_runtime::session'`
  across the four crates gives the full worklist; nothing in this doc's
  survey found a call site that mixes a `session::` path with a non-session
  `tidepool_runtime::` path on the same `use` line, so the rename should not
  need per-line judgment calls).

### 4.3 Incremental order

1. **Move the nine files verbatim** into a new `tidepool-session` crate
   (`Cargo.toml`, `src/lib.rs` re-declaring the nine `pub mod`s exactly as
   `session/mod.rs` does today at `mod.rs:21-28`, same `pub use` surface at
   `mod.rs:29-56`). Widen `tidepool_toolchain::extract_module_name` from
   `pub(crate)` to `pub` (§1.3's one precedented visibility change) and
   point every toolchain-shim edge (`paths`/`diag`/`toolchain`/
   `CompileError`) directly at `tidepool_toolchain::*` instead of the
   `tidepool_runtime::*` re-export. Point every facade-type edge
   (`EvalResult`/`RuntimeError`/`JitError`/`EVAL_STACK_SIZE`/
   `DEFAULT_NURSERY_SIZE`) at the new `tidepool-runtime` dependency. No
   behavior change — this step is a pure file move plus import-path
   surgery, oracle is both crates' existing test suites, byte-identical.
2. **Update the four consumer crates** (§1.4) per §4.2's mechanical rename.
   Each crate's own test suite is the oracle; land one crate at a time
   (smallest first — `tidepool-mcp`'s `SessionLib`-only dependency is the
   narrowest surface) rather than all four in one commit, so a broken
   rename in one crate doesn't block validating the others.
3. **Retire `plans/resident-session-kernel-design.md`.** Per its own §4.4/
   Open-Question-2 answer ("the packaging call is therefore an
   implementation-time judgment... let §7's multi-frontend requirement tip
   it if the module form would force a frontend to reach through non-public
   internals") and this doc's §4.1 crate-name decision, that doc's crate-
   vs-module question is now answered by whichever lane executes step 1-2
   above. Anything from it still load-bearing after that — the kernel's own
   design rationale (§3 of that doc), the Phase-6/slot-path fork status,
   the answered Open Questions — belongs hoisted into `tidepool-session`'s
   new `CLAUDE.md` (a charter section modeled on `tidepool-toolchain/
   CLAUDE.md`'s own, naming what belongs/doesn't) and `tidepool-harness/
   CLAUDE.md`/`tidepool-repl/CLAUDE.md`'s existing "Internals" sections,
   which already cite `resident-session-kernel-design.md` by name and
   should cite the new crate's own docs instead once it exists. **This doc
   does not delete the plan file itself** — per the spec's own instruction,
   that deletion rides the implementation lane that actually executes
   steps 1-3, not this design doc.
4. **Multi-mount (§2) is a SEPARATE, later lane**, not part of steps 1-3.
   Nothing in the promotion requires an admission-model decision first —
   `SessionRegistry`/`kernel`/`ResidentSession` move as-is, unchanged in
   behavior, and the mounting-model work (§2.4's fork) lands as new code
   against the already-promoted crate. See Open Question 1 for whether the
   operator wants this sequencing reconsidered.

### 4.4 What breaks for harness/repl thin clients at each step

- **Step 1** (the crate move itself): nothing breaks for consumers — it is
  entirely internal to the new crate's own compile, verified before step 2
  starts.
- **Step 2, per consumer crate**: a compile error at every renamed import
  path until that crate's own edit lands (expected, mechanical, not a
  design risk) — no RUNTIME behavior changes, since nothing about
  `SessionRegistry`/`ResidentSession`/`SessionEngine`'s own logic moves or
  changes, only which crate they live in.
- **Step 3** (doc retirement): no code effect at all.
- **A harness/repl thin client sees ZERO multi-mount-related change** at
  any step in this migration — §2's admission model, frontend identity, and
  wedge-attribution gaps are all still open after step 4, by design (§4.3
  point 4). The promotion is a precondition for building multi-mount
  cleanly, not a delivery of it.

---

## Decisions (operator, 2026-08-24)

1. **Sequencing: promote first, WITH the identity parameter reserved.**
   §4.3 step 1 carries one additional constraint: the checkout API grows a
   frontend-identity parameter now (threaded, unused by admission logic
   yet) so the mount wave widens no registry signatures a second time.
2. **Mounting model (when mount work starts): Option 3 composed with
   Option 1 at TURN granularity.** Scope-per-frontend for isolation (§2.4's
   own finding that it is load-bearing regardless), leases acquired per
   TURN via `admit_checkout` — no frontend monopolizes across turns.
   Upgrade to Option 2's FIFO dispatcher only on observed starvation.
3. **Frontend identity: a typed `FrontendId` newtype owned by
   `tidepool-session`**, with boundary constructors from harness `NodeId`
   and MCP connection ids. The suspension-routing key (I4) extends with
   this one typed field.
4. **Crate name: `tidepool-session`** (as recommended; the two rejections
   stand).

## Open questions (retired — answered above)

1. **Does multi-mount (§2) land as part of this promotion, or strictly
   after it (§4.3 point 4's assumption)?** This doc assumes "after" —
   promote first, mount later — on the reasoning that §2's admission-model
   fork (Option 1 vs. 2 vs. composing with 3) is genuinely unresolved and
   should not gate a mechanical, low-risk crate move behind it. If the
   operator wants multi-mount's admission model decided (even if not built)
   BEFORE the promotion lands, §4.3's step 1 should carry that decision as
   an additional constraint on the registry's API shape (e.g., reserving a
   frontend-identity parameter on `checkout_run` now rather than widening
   it later) — a real, if narrow, scope change to this doc's step 1.
2. **Which mounting-model — per-frontend leases (Option 1), turn-admission
   queueing (Option 2), or Option 3's scope-forking composed with whichever
   of the first two — does the operator want built first, when multi-mount
   work eventually starts?** §2.4 lays out the tradeoffs (Option 1 simplest,
   monopolizing; Option 2 fairer, needs a new stateful dispatcher; Option 3
   orthogonal to both, likely load-bearing regardless) but does not pick.
   §2.5's wedge-attribution gap is downstream of this answer.
3. **What identifies a frontend (§2.6)?** A bare string, a typed newtype
   crate-local to `tidepool-session`, or reuse of an existing identity
   (a harness `NodeId` when the frontend IS a harness node; an MCP
   connection id for a repl-mounted or network-mounted one)? This touches
   vocabulary in both `tidepool-harness` and `tidepool-mcp` and is flagged
   rather than assumed per the Dev Agent Protocol's own "don't resolve an
   ambiguity that touches another crate's vocabulary" guidance.
4. **Crate name (§4.1)** — this doc recommends `tidepool-session` and
   argues against the two alternatives it considered. Confirm or override.
