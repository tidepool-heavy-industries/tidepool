# Spec: GC-root the stowed continuation — nested child runs on a suspended machine

Goal: while a parent session is suspended at `returnControl`, run CHILD
fragments on the SAME machine (reading parent bindings zero-copy) without
the child's GC moving the parent's continuation out from under it. This is
the only memory-safety-critical segment in R0. **Starts after segment 20
lands. Opus implements; fable reviews every diff at the merge boundary.**

## ANTI-PATTERNS

- DO NOT delete the L7 asserts — CONVERT them into an explicit supported
  nested mode (an enum/flag on the machine: `NestedChildRun { parent_cont
  rooted }`), so the illegal states (run with unrooted live continuation)
  remain unrepresentable or at minimum still assert.
- DO NOT root the continuation by copying it — register the SLOT
  (`&mut suspended_continuation as *mut *mut u8`) so the collector's
  in-place slot rewriting updates it on relocation.
- DO NOT let a child's turn-boundary reclaim clobber the parent's heap
  bookkeeping — the cursor/high-water-mark handoff must nest (see step 3).
- DO NOT touch the parallel-fork machinery (heap copy-out, pipeline
  sharing) — R2, out of scope.
- DO NOT ship without the adversarial suite green under
  `scripts/battery.sh`; unit tests alone are insufficient here (repo GC
  history: repo-review-2026-07-06 findings demanded poison/verify tests).

## READ FIRST

- `tidepool-codegen/src/jit_machine.rs`: `suspended_continuation` +
  its safety comment (103–110: "no GC runs on a suspended machine" — the
  temporal argument THIS segment replaces with a root); the L7 comment
  (2239–2247: "not a GC root … safety rested entirely on external
  discipline"); the assert sites (511, 603, 776, 954, 1087, 1232, 1387);
  `RegistryGuard`/`reclaim_session_heap` (240–280); `drive_effect_loop`'s
  arm-scoped `RootScope`/`register_rust_root` (1770–1790) and
  `materialize_response_and_resume` re-rooting (1890–1893) — the existing
  patterns for rooting a continuation during driver-loop allocation.
- `tidepool-codegen/src/host_fns/gc.rs`: `perform_gc` root assembly
  (525–577 — the four root sources: frame-walked stack, run-scoped
  rust_roots, session persistent_roots, vmctx tail-call slots); heap
  doubling's two-pass copy (602–624); swap (637–640).
- `tidepool-heap/src/gc/raw.rs`: `cheney_copy` (152–190),
  destructive forwarding (`evacuate`, 37–40).
- `tidepool-codegen/src/old_space.rs`: `RootSlot` contract (64–75),
  persistent root cells (126).
- Segment 20's landed code (the nested mode extends its registry states).

## HANDOFF FROM SEGMENT 20 (landed 9faa78a2 — read these files first)

- The exact seams to replace: `ResidentSession::run`'s `Suspended`
  rejection (`ResidentError::Suspended`) in
  `tidepool-runtime/src/session/resident.rs`, and
  `SessionRegistry::checkout_run` rejecting `Slot::Suspended` in
  `tidepool-harness/src/registry.rs`. Nested child runs replace those
  rejections with child-fragment runs against the suspended machine.
- CONTRACT CHANGE REQUIRED: `resume_suspended` `.take()`s the stowed
  continuation ON ENTRY regardless of outcome, and
  `ResidentSession::reenter` clears `pending` up front to match. A
  nested-run design must PRESERVE the continuation across child runs —
  that consume-on-entry contract changes here, deliberately.
- Segment 20 proved cross-turn state via the effect plane only (resident
  KV). Value-plane tenure across a suspension (a session `RootSlot`
  referenced by a later fragment's `ExternalEnv`) was explicitly deferred
  to THIS segment — add it to the adversarial suite: parent binds a
  value, suspends, child forces GC, parent resumes and reads the binding.
- `run_fragment_suspendable` / `run_suspendable_with_entry` exist in
  jit_machine.rs (segment 20's factoring) — build on them.

## MECHANISM / STEPS

1. **Root registration**: entering nested-child mode registers the
   suspended continuation slot in `persistent_roots` (or a dedicated
   `stowed_roots` set folded into `perform_gc`'s root assembly — prefer a
   dedicated set so intent is auditable); leaving the mode (parent resume
   or child teardown) deregisters. The slot is rewritten in place by the
   collector on relocation — after any child GC, the parent's field points
   at the moved continuation.
2. **Assert conversion**: run entries accept `NestedChildRun` mode iff the
   continuation slot is registered; the old asserts remain for the
   unregistered case. Registry (segment 20) gains the
   `Suspended → RunningChild → Suspended` transitions; parent resume is
   rejected while a child is mid-run (sequential-isolated means exactly
   one computation on the heap at a time).
3. **Reclaim/cursor nesting audit**: a child turn's `RegistryGuard::drop`
   reclaims the session heap and records the allocation high-water mark —
   verify the parent's stowed state observes the POST-child heap (buffer
   may have been swapped/doubled by child GC) and cursor. Read the
   reclaim-ordering comments in `run_pure_and_bind` (this area has bitten
   before); document the nesting invariant in the module docstring.
4. **NF-force primitive** (A5, same neighborhood): a deepseq-style walk
   forcing a data-kinded answer value to normal form before it is
   materialized into the parent's resume — a bottom anywhere fails the
   answer WITHOUT consuming the continuation (surfaces as the retry
   error). Walk terminates on visited-set (cyclic data). Function-bearing
   types were rejected at extract (segment 10), so every field is
   walkable by construction.
5. **Adversarial suite**: tests that (a) allocate in a child until GC
   fires with a live suspended parent, then resume the parent and deep-
   verify the continuation's captured values (poison from-space via the
   existing debug knobs where available); (b) child triggers heap
   DOUBLING mid-run, parent resumes; (c) child defines new decls
   (`add_function`) then parent resumes and calls nothing new (module
   accretion is inert for the parent); (d) `resume undefined`-shaped
   bottom answers do not consume; (e) nested-mode misuse (parent resume
   during child run) errors cleanly.

## VERIFY

- Adversarial suite green under `cargo nextest run` AND
  `scripts/battery.sh` (process-per-test matters here).
- Existing eval-server + repl suites untouched and green.
- Fable review of every diff touching `unsafe`, root assembly, or
  reclaim ordering before merge.

## DONE

A parent suspended at `returnControl` hosts an arbitrary number of
sequential child fragment runs — including ones that force GC and heap
doubling — and resumes correctly afterward; bottoms don't consume; the
temporal "no GC while suspended" argument is replaced by a registered
root, documented in the module docstring.
