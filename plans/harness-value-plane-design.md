# W1b-redux Step 1 — within-node value-plane materialization (design)

## ✅ COMPLETE (2026-08-01) — commits a38d33ee, 1046c7c1, 0d4be565, 088ec27a

An effectful value bind persists across turns. `acceptance_value_bind` (turn 1
`steps <- returnControlFork @[Int]` → answer `[1,2,3]` → turn 2 `total <- pure
(sum steps)` renders `6`) passes end to end through the real
`drive_turn → run_bind → answer_fork → resume_bind → materialize → inject-val`
path. Harness regression 18/18, codegen 601/601 under GC poison, resident_session
4/4 — no regression. Scope landed: BIND turns (the common value-persistence path).
Follow-ups (NOT step 1): expr turns referencing a value binding (route the expr
path through `compile_session_turn` too — deferred for expr-render parity);
multi-bind `(a,b) <- e`. Steps 2 (decl-source fork inheritance) and 3 (value
heap-clone on fork) remain per the main plan.

## Fork inheritance (steps 2–3) — DEFERRED; decision recorded (2026-08-01)

**"Good-enough fork" today:** the current fork/answerer flow (child inherits the
parent's TRANSCRIPT, answers, resumes the parent — `golden_path`/`acceptance_forkall`
green) is the good-enough state. Full session inheritance (a forked child seeing the
parent's decls + value bindings as LIVE bindings) is deferred. Single-node work
(resume + forms + value persistence — now landed) does not need it.

**Snapshot mechanism = (B), re-bootstrap** (Inanna, 2026-08-01). "Bootstrap" =
`JitEffectMachine::compile_session` builds a machine with its OWN JIT-compiled code
module (the `pipeline`/Cranelift `JITModule`). **(A)** = the forked child SHARES the
parent's one code module (cloned closures' code pointers stay valid, but two
machines mutating one module while the parent is parked is fraught). **(B)** = the
child RE-BOOTSTRAPS its own fresh machine/module from the boot seed. Chosen because
it lets **N forked children each independently diverge** from the parent's context
without stepping on each other or the parent — the multi-child-sharing-parent-ctx
goal. Decomposition under (B), by substrate:
- transcript → copy (trivial).
- decl plane → the child re-imports the parent's `Lib.G` SOURCE and recompiles in
  its own module (B-native; no heap surgery).
- value plane Tier0 (forced data, e.g. `[Int]`) → clone the roots into the child's
  fresh heap (no code pointers; `RootSlot` addrs rebase into the child's machine).
- value plane Tier1 (a bound closure/PAP) → the ONE hard case: embeds a code pointer
  into the PARENT's module. For v1, RESTRICT inheritance to Tier0 + decls (a forked
  child cannot inherit a live parent closure) — loud, not silent — and revisit if a
  real case needs it. This is the open detail to settle when steps 2–3 are built.

---

Status: DESIGN COMPLETE, implementation in progress (single-threaded, by hand).
This is the authoritative spec; a prior delegated attempt failed by splitting the
bind across the async boundary (forcing `!Send` `RootSlot`/`ExternalEnv` through
`spawn_blocking` → E0277). The correct design keeps ALL `!Send` work on the eval
thread and lets only the `Send` machine carry the `RootSlot` home.

## Goal

An effectful bind turn `x <- e` (whose RHS SUSPENDS at a fork under Threadless)
persists `x` as a live typed binding into the node's LATER turns. Within ONE node
across its own turns — NOT fork inheritance (that is step 2/3).

## The crux the delegated agent smashed into

- A harness turn runs on `spawn_blocking` (`harness.rs:756`): the whole `session`
  (Send) moves into the closure and back.
- The repl materializes a bind with `run_fragment_and_bind` → returns a `RootSlot`
  (`*mut *mut u8`, `!Send`) ON ITS PINNED THREAD (no boundary crossing).
- The harness's `on_eval_thread` (`resident.rs:444`) runs the machine on a scoped
  eval thread. A `RootSlot` cannot cross OUT of that scope as a bare value (`!Send`).
- AND a fork bind ALWAYS suspends — `run_fragment_and_bind` (non-suspendable,
  asserts `suspended_continuation.is_none()`) can't span a Threadless suspension.

## The GC-ordering insight (what makes it correct)

`run_fragment_and_bind` (`jit_machine.rs:1229`) tenures INSIDE the drive epilogue
and arms reclaim LAST (comment at 1265-1268 / 1335-1345): tenure touches
`self.session`; `arm_reclaim` stores an aliasing `*mut self.session`; the two
cannot coexist. The suspendable entries (`run_suspendable_with_entry:752`,
`resume_suspended:835`) currently arm reclaim BEFORE driving.

Fix: move `arm_reclaim` to AFTER `finish_suspendable` in BOTH suspendable entries.
Safe for the non-bind case too — nothing touches `self.session` between
install_registries and the (now-later) arm, exactly as `run_fragment_and_bind`
already does (guard created, drive with GC, arm last). Then `finish_suspendable`
tenures on `Done` when a bind flag is set, before the deferred arm.

## jit_machine.rs changes (minimal)

1. New field `last_bound_root: Option<crate::old_space::RootSlot>` on
   `JitEffectMachine` (init `None` in the constructor(s) that set
   `suspended_continuation`). Consistent with the machine's existing raw-pointer
   fields + `unsafe impl Send` (stowed-XOR-running). Add `take_last_bound_root(&mut
   self) -> Option<RootSlot>`.
2. `finish_suspendable(&mut self, machine, outcome, bind_forced: Option<bool>)`:
   - `bind_forced = None` → today's behavior (bridge `Done` ptr → `Completed(value)`;
     stow on `Suspended`). Byte-identical.
   - `bind_forced = Some(forced)` on `Done(done_ptr)`: deep_force if `forced`
     (mirror 1287-1306), tenure into `self.session.old_space.tenure(vmctx, nf_ptr,
     from_range)` (mirror 1308-1332) → `self.last_bound_root = Some(slot)`, then
     bridge `slot.current()` (rooted, stable) → `Completed(value)`. On `Suspended`,
     identical stow (no tenure — the bind completes on a later resume).
3. Move `arm_reclaim` to after the `finish_suspendable` call in both
   `run_suspendable_with_entry` and `resume_suspended`, and thread `bind_forced`
   into both (default `None` from the existing public `run_suspendable`/
   `run_fragment_suspendable`/`resume_suspended`; new `*_binding` public wrappers
   pass `Some(forced)`).
   - `run_fragment_suspendable_binding(func_id, table, handlers, user, tag, forced)`
   - `resume_suspended_binding(table, handlers, user, tag, input, forced)`

## persistent.rs / Threadless

Add core methods that call the machine's `*_binding` entries (Threadless only;
ParkedThread/repl keep `run_fragment_and_bind`):
- `run_funcid_suspendable_binding(func_id, handlers, captured, forced) -> SuspendableOutcome`
- `resume_binding(handlers, captured, input, forced) -> SuspendableOutcome`
After a `Completed`, the RootSlot is on the machine; caller reads it via a core
method `take_last_bound_root() -> Option<RootSlot>` (delegates to `machine_mut`).

## resident.rs

- `run_bind(name_hint, expr, table, forced) -> ResidentOutcome`: on the calling
  thread `merge_table` + `seed_external_env` (from `self.core.bindings`) +
  `add_fragment_session` → func_id; `on_eval_thread` runs
  `run_fragment_suspendable_binding`; after restore, if `Completed`, take the
  RootSlot off the machine and return it to the harness (the harness records the
  `BindingEntry`, since it owns binder metadata). If `Suspended`, arm `pending` as
  usual — the RootSlot lands on the eventual resume.
- `resume_bind(cont_id, answer, forced)`: like `resume`, but drives
  `resume_suspended_binding`; on `Completed`, surface the RootSlot.
- Both surface the RootSlot to the harness via a new `ResidentOutcome` field or a
  `last_bound_root()` accessor read after the call (the RootSlot is `!Send` but
  stays in-process on the calling thread — fine; it never crosses `spawn_blocking`
  again because materialize records it into the Send BindingTable synchronously).

## STATUS

- **[DONE `a38d33ee`] jit_machine.rs primitives** — `run_fragment_suspendable_binding`
  / `resume_suspended_binding` / `take_last_bound_root` + `finish_suspendable(bind_forced)`;
  arm_reclaim moved after finish. 601/601 codegen under `GC_POISON`+`HEAP_VERIFY`.
- **[DONE `1046c7c1`] resident.rs bind path** — `run_bind`/`resume_bind`/
  `materialize_binder`; `run` seeds env internally; `val_gen`/`inject_val_modules`
  accessors; `reenter` threads `(binder, gen)`. Verified: resident_session 4/4,
  golden_path + cross_turn + form_widgets green (real extract).
- **[DONE `0d4be565`] compile_session_turn returns `asks`** — SessionTurnResult
  gains the typed-yield sidecar so the harness can adopt it.
- **[REMAINING] the harness turn-path wiring** (the last mile — delicate, on the
  working fork/dialog/decl path; verify with golden_path + cross_turn after each
  change):
  1. Expose the node's session_root on `ResidentSession` (the `SessionLib` root
     `compile_session_turn` writes ifaces to) — a `session_root()` accessor over
     `core.lib_include_dir()` (or the SessionLib root).
  2. `run_block`: replace `compile::compile_turn` with `compile_session_turn`
     (session_root + inject = decl module ++ `inject_val_modules()`), reading the
     `asks` off `SessionTurnResult` for hole classification (replaces the bespoke
     `compile.rs` path). Decl turns still go through `define_scoped` (unchanged).
  3. Classify Bind (`classify_turn` → `TurnKind::Bind`): mint `g =
     session.val_gen().next()`, wrap `__result = do { <stmt>; pure <names> }`,
     compile with `SessionBind{names, gen: g.0}`, call `run_bind(.., binder, g)`.
     On `Completed` → `TurnOutcome::Completed` (render "bound x"); on `Suspended`
     → stash `(binder, g)` in `NodeConvo` beside `pending`, return `Suspended`.
  4. `resume_parent` (the path `answer_fork`/`answer_fanout` call): if the node
     has a stashed pending-bind, drive `resume_bind(cont, answer, binder, g)`
     instead of `resume`; materialize lands on its `Completed`.
  5. Acceptance test `acceptance_value_bind.rs` (real extract, `GC_POISON`):
     turn 1 `steps <- fork @[Int] "…"` → answer `[1,2,3]`; turn 2
     `pure (toJSON (sum steps))` renders `6`.

## REFINEMENT 1 — the value plane FORCES unifying harness compile onto `compile_session_turn`

The harness's own `compile.rs::compile_turn` (target `result`, no binders, no
session-injection, but reads `asks.json`) CANNOT resolve value bindings: a `Val.G<g>`
binding is a THIN IFACE resolved at runtime via `ExternalEnv`, needing the extract's
`--inject-val <module>` (compile-side) + `seed_external_env` (runtime-side). Textual
import (W1b's decl mechanism) can't carry a value. So the harness turn compile MUST
adopt `tidepool-runtime::session::turn::compile_session_turn` (`--session-root` +
`--inject-val` + `--session-bind`/`--emit-bound-binders`), the SAME path the repl
uses — which IS the W1 unification goal, not a detour.
- The node already has a `SessionLib` (from `node_decl_plane`) → its root is the
  `session_root`; `current_val_modules`+`current_lib_module` → `inject_modules`.
- `compile_session_turn` does NOT read `asks.json` today; the extract still WRITES
  it to the temp dir. EXTEND `SessionTurnResult` with an `asks` field (read the
  sidecar from the same temp dir, mirroring `compile.rs::load_asks`). Then the
  harness gets binders + asks + session-injection from ONE call, and
  `harness/src/compile.rs`'s bespoke path can retire (or shrink to the sidecar
  parser it shares).
- `classify_turn` (turn.rs, already `pub`) replaces the harness's decl-only check:
  Decl → decl plane; Bind → value-plane bind path; Expr → session-aware expr.

## REFINEMENT 2 — generation threading (compile-time gen == materialize gen)

The `Val.G<g>` gen is minted BEFORE compile (`g = core.val_gen().next()`), passed
as `SessionBind{ gen: g.0 }`, and the extract stamps `binder.module =
"Tidepool.Session.Val.G<g>"`. Materialize MUST reuse that SAME `g`
(`SessionModule::val(g)` + `set_val_gen(g)`), not re-mint. So the harness mints `g`
at compile, threads it into `run_bind` AND (for a fork bind that suspends) into the
remembered pending-bind for `resume_bind`. `run`/`run_bind` should SEED the external
env internally from `self.core.bindings` (the session owns its env) rather than take
an empty `ExternalEnv` param — that is what makes a turn-2 expr resolve `steps`.
Materialize (in resident.rs, mirroring the repl's session.rs layer — NOT the core):
take the RootSlot off the machine, `retract` any same-name decl (one-plane
invariant), `bind(BindingEntry{ name, id: SessionVarId::from_extract(var_id),
module: SessionModule::val(g), value: tier→BoundValue, type_display })`,
`set_val_gen(g)`.

## compile.rs (superseded by Refinement 1)

Do NOT grow the bespoke `compile.rs`. Adopt `compile_session_turn` + extend it with
`asks`. `compile.rs` keeps only what's still harness-specific (if anything).

## harness.rs

- `run_block`: after the decl check, classify a single effectful bind `x <- e`
  (extend `classify_turn`/`TurnKind`; extract binder name(s)). Route to a bind path
  that wraps the source (`__result = do { <stmt>; pure <binders> }`), compiles with
  binder metadata, and calls `session.run_bind`.
  - `Completed` → record `BindingEntry{name, id: SessionVarId::from_extract(var_id),
    module: SessionModule::val(g), value: bound_value(tier, slot), type_display,
    defining_expr}` into the session's BindingTable via a core `bind()` call; render
    "bound x :: T".
  - `Suspended` (fork) → stash the pending-bind (name, var_id, tier, gen) alongside
    `NodeConvo.pending`, return `Suspended`. `answer_fork`/`answer_fanout` →
    `resume_parent` re-enters via `resume_bind`; on the resulting `Completed`,
    materialize using the stashed pending-bind, THEN return `Completed`.
- Extend `session_decl_context` (the W1b decl peek) to ALSO import the node's
  current `Val.G<g>` modules + add their include dir (`current_val_modules` /
  `live_val_modules` / val include dir), so turn 2's `forkAll [f s | s <- steps]`
  resolves `steps`.

## Test (real entry point)

`tidepool-harness/tests/acceptance_value_bind.rs`: mirror `acceptance_cross_turn`.
Turn 1 replies `steps <- fork @[Int] "give me [1,2,3]"`; drive to the fork hole,
answer with `[1,2,3]`. Turn 2 `pure (toJSON (sum steps))` MUST render `6`. Gate on
`TIDEPOOL_EXTRACT`. Also assert `acceptance_cross_turn` + `golden_path` still green,
and the repl suite (regression oracle).

## GC test gate (mandatory per tidepool-codegen/CLAUDE.md)

Run the new bind path under `TIDEPOOL_GC_POISON=1` + `TIDEPOOL_HEAP_VERIFY=1`
(the `nested_child_gc_rooting.rs` regime): tenure-on-resume across suspend → child
answer → resume must not corrupt the heap. Add a probe if the acceptance test
doesn't exercise a GC during the suspended window.
