# W1b-redux Step 1 — within-node value-plane materialization (design)

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

## compile.rs

`compile_turn` must emit stmt-binder metadata for a bind turn (var_id, tier
`ValueTier`, type_display) — mirror `tidepool-runtime`'s `compile_session_turn` /
`SessionBind` extract flag. Add a `compile_bind_turn` (or a `binders: Option<&[..]>`
param) that passes the flag and reads the binder cbor/json. Reuse
`tidepool-runtime`/`tidepool-codegen` binder+tier types (widen to `pub` narrowly
if needed) — do NOT duplicate.

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
