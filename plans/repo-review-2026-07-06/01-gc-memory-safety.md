# 01 — GC / memory safety (tidepool-codegen + tidepool-heap)

The dominant theme of the codegen/heap review: **the Cheney GC's correctness
contract — "every live slot a scan can reach is either null or a valid pointer,
and every heap pointer the GC can't see must not be live" — is violated in
several distinct ways.** Findings 1–4 are all instances. Fresh semispaces are
zeroed, so these are latent until a few collections recycle memory — exactly
the long-lived repl-session profile. If you're hunting "rare nondeterministic
corruption in a long session," it's one of these.

Two reviewers found finding 2 (boxed arrays) independently; findings 1–4 were
re-verified by direct read in the coordinating session.

## ANTI-PATTERNS (read first)

- Do NOT flag/"fix" locked decisions: HeapObject is raw byte buffers + unsafe
  accessors (not an enum); the GC is a copying collector with a custom RBP
  frame walker. See root CLAUDE.md Key Decisions Reference.
- Do NOT rely on the existing test suites to prove these fixed — the array
  proptest suite explicitly models boxed slots as "opaque tokens the host
  never dereferences" (`tidepool-testing/tests/proptest_host_arrays.rs:24-32`)
  and the post-GC verifier classifies array payloads "outside both spaces —
  allowed" (`host_fns/gc.rs:296`). Write the red test FIRST for each finding.
- Do NOT zero-init by adding per-site stores ad hoc — extract one
  allocate-and-zero helper (see Fix Strategy under finding 1) so the bug class
  is unrepresentable.

## READ FIRST

- `tidepool-codegen/CLAUDE.md` (diagnostics: `RUST_LOG=tidepool::heap=trace`,
  `TIDEPOOL_HEAP_VERIFY=1` post-GC verifier lane)
- `tidepool-heap/src/gc/raw.rs` — `for_each_pointer_field` + `cheney_copy`
- `tidepool-codegen/src/emit/expr.rs:2572-2580` — the LetRec Con pre-alloc
  that already zero-inits, with a comment naming the exact hazard of finding 1
- `tidepool-codegen/src/host_fns/gc.rs` — `perform_gc`, buffer swap at :537
- `tidepool-codegen/src/old_space.rs` — tenure + `measure_closure_bytes`

---

## Finding 1 (CRITICAL): allocate-then-fill emit paths leave counted slots uninitialized across GC points

**Where:** `tidepool-codegen/src/emit/expr.rs` — four sites:

- **1a `ThunkCon` arm (:491-566)** — VERIFIED BY DIRECT READ. The Con is
  allocated via `emit_alloc_fast_path` and `num_fields = N` stored BEFORE the
  per-field loop; each iteration calls `emit_node`/`emit_thunk`, both GC
  points. Slots are never nulled. The Con ptr is stack-mapped
  (`declare_value_needs_stack_map`), so the collector scans N slots of stale
  bump-heap bytes; any stale value aliasing from-space gets "evacuated"
  (garbage size read, forwarding word written into a live object).
  Scenario: `Just (f x)` — field-0's thunk alloc triggers GC mid-fill.
- **1b LetRec Phase-1 `Lam` pre-alloc (:2506-2539)** — capture slots aren't
  nulled until Phase 3a (:2769-2776), but the NEXT binding's pre-alloc is a GC
  point in between. Any mutually-recursive group (`even`/`odd`) can hit it.
- **1c `emit_lam` capture fill (:1352-1398)** — the fill loop calls
  `ensure_heap_ptr` per capture, which ALLOCATES a Lit when the capture is
  `SsaVal::Raw` (e.g. `let n = x +# 1 in \y -> n + y`) — a GC point while the
  remaining slots are garbage.
- **1d `emit_thunk_promised` (:1582, :1626-1649)** — same pattern; the
  `Some(ssaval)` fill path is the hole (promised slots correctly store null).

**Fix strategy:** extract ONE emit helper that allocates, stores the header +
count, and zero-fills all slots in a single emit sequence; use it at all four
sites (and at the LetRec pre-alloc, replacing its hand-rolled zero-init).
Alternative for 1a only: evaluate all fields before allocating, like the plain
`Con` arm — but the shared helper kills the whole class.

**Verify:** red test per site is hard to write deterministically at the unit
level; instead (a) run the heap-verify lane (`TIDEPOOL_HEAP_VERIFY=1`) over the
differential corpus — the verifier DOES flag garbage in counted slots when a GC
lands mid-fill; (b) add a stress test that compiles `Just (expensiveThunk x)`
shapes with the nursery limit forced tiny (see how existing GC-trigger tests
shrink the nursery) so the mid-fill GC is deterministic.

> **STATUS: FIXED** (2026-07-07). Extracted `emit_alloc_zeroed` (`emit/expr.rs`,
> right after the imports) — allocates, writes the tag/size header, and
> zero-fills `[fields_offset, fields_offset + n_slots*8)` in one emit sequence
> with no GC point in between. Used at all 4 named sites (`ThunkCon` arm,
> `emit_lam` capture fill, `emit_thunk_promised`, LetRec Phase-1 `Lam`
> pre-alloc) plus the LetRec Phase-1 `Con` pre-alloc (already-correct
> hand-rolled zero-init, folded into the same helper for one source of truth).
> The redundant Phase-3a capture zero-init (the one that ran too late to matter)
> was deleted as dead code.
>
> **Discrepancy (trust the code):** the red test could NOT be made to fail
> pre-fix. Every nursery/tospace/old-space buffer in this codebase is always
> allocated via `vec![0u8; n]` (a zeroing allocation) — the initial `Nursery`,
> every GC `tospace`, the heap-doubling buffer, and `OldSpace`'s arenas. Bump
> allocation only ever writes forward within one buffer generation, so an
> unfilled slot is genuinely `0`/null (a value the verifier and `cheney_copy`
> both already treat as "legal, deferred field"), never non-zero garbage —
> under THIS allocator design the described hazard is latent-but-currently-
> unreachable, not live. The fix is still correct and worth keeping: it's the
> load-bearing invariant that makes any future move to a non-zeroing
> allocation strategy (e.g. `Vec::with_capacity`+`set_len` to skip the
> zeroing cost) safe by construction instead of silently reintroducing this
> class. Verified via `tidepool-codegen/tests/con_midfill_gc_safety.rs`
> (stress test, `TIDEPOOL_HEAP_VERIFY=1` forced on, passes green; a temporary
> revert of the `ThunkCon` site's zero-fill was confirmed to still pass too,
> confirming the red-test gap rather than a fix regression) plus the full
> `TIDEPOOL_HEAP_VERIFY=1` differential lane.

## Finding 2 (CRITICAL): boxed `SmallArray#`/`Array#` element pointers invisible to GC — UAF after one collection

**Where:**
- `tidepool-heap/src/gc/raw.rs:56-107` — `for_each_pointer_field` handles
  TAG_CLOSURE/TAG_CON/TAG_THUNK; `_ => {}` for TAG_LIT (verified by read).
- `tidepool-codegen/src/host_fns/primops.rs:268` — `runtime_new_boxed_array`
  mallocs `[u64 len][ptr0..ptrN]` OUTSIDE the GC heap, wrapped in a nursery Lit.
- `tidepool-codegen/src/emit/primop.rs:2134-2180` — `newSmallArray#` stores the
  init value's HEAP POINTER into every slot; `readSmallArray#` loads and
  stack-maps the result.
- `tidepool-codegen/src/old_space.rs:245-289` — `measure_closure_bytes` has the
  same TAG_LIT blindness, so a tenured value containing an array keeps nursery
  element pointers across minor GCs.

**Failure:** `newSmallArray# n x` → any later allocation fires `gc_trigger` →
`cheney_copy` evacuates live objects and the old nursery `Vec<u8>` is dropped
(`host_fns/gc.rs:537` `state.active_buffer = Some(active)`) → array slots still
hold from-space addresses into freed memory → `readSmallArray#` /
`indexSmallArray#` dereference freed memory → garbage or SIGSEGV.

This is correct-by-design for `ByteArray#` (payload deliberately malloc'd
outside the nursery, `heap_bridge.rs:545`, and bytes contain no pointers) but
NOT for pointer-element arrays. Nothing registers the slots as roots
(`rust_roots` is the apply-cont stack; persistent roots are tenured bindings;
no array registry exists). Dormant ONLY because the current stdlib happens not
to store heap values in boxed arrays across GC points — an invariant nothing
enforces or states. Any `unordered-containers HashMap` / `Data.Primitive` /
KeyMap-shaped stdlib work detonates it.

**Fix options (pick one, first is least invasive):**
1. Teach `for_each_pointer_field` + `measure_closure_bytes` to walk
   `LitTag::SmallArray/Array` payload slots — payload is malloc'd/stable, so
   only the SLOT CONTENTS need evacuating/updating. The from-space range check
   in `cheney_copy` makes over-tracing safe.
2. Per-machine array registry appended to GC roots.
3. Allocate boxed arrays as a real scanned in-heap object kind (dovetails with
   the malloc-leak opportunity below).

**Verify (write FIRST, must be red):** newArray with a Con element → force a
GC (tiny nursery or alloc pressure) → readArray → assert the element is intact.
Then extend `verify_heap_post_gc` to walk array payload slots (it is currently
structurally blind: gc.rs:296), and run an array-exercising corpus through the
heap-verify lane.

> **STATUS: FIXED** (2026-07-07). Fix option 1 (locked) implemented: added a
> `TAG_LIT` arm to `for_each_pointer_field` (`tidepool-heap/src/gc/raw.rs`) that,
> for `LitTag::SmallArray`/`Array`, reads the payload pointer at
> `LIT_VALUE_OFFSET`, reads its length prefix, and calls `f` on each element
> slot address — the payload buffer itself is never evacuated (stable,
> GC-external), only its slot CONTENTS are traced/updated, exactly as the from-
> space range check in `cheney_copy` already makes safe for any address outside
> both semispaces. This single fix ALSO fixes `measure_closure_bytes`
> (`old_space.rs`) for free — it calls the same `for_each_pointer_field`.
> Extended `verify_heap_post_gc` (`host_fns/gc.rs`) with the matching walk over
> array payload slots (from/to-space pointer checks, same as any other field),
> replacing the old "outside both spaces — allowed" blind spot for this case.
>
> **Verified red-then-green**: `tidepool-codegen/tests/array_gc_safety.rs`
> (NEW) drives `newSmallArray#`/`indexSmallArray#` through the real JIT path
> with a genuine heap `Con` element and a tiny (2 KiB) nursery, under a
> self-recursive allocating loop that forces several real collections between
> the array's creation and its final read, while the element's OWN binding is
> never referenced again (the array slot is its only remaining root). Confirmed
> RED before the fix (reverting just the `raw.rs` change aborts the process
> inside `gc_trigger` — the heap verifier catches the corruption before it can
> silently misread); confirmed GREEN after. This is the one finding whose
> worst case reproduced exactly as described.

## Finding 3 (CRITICAL): `parse_result` holds unrooted heap pointers across GC-capable forces

**Where:** `tidepool-codegen/src/effect_machine.rs:246-336` (E arm).

`result`, `continuation`, `union_ptr`, `tag_ptr`, `request` are held raw across
multiple `force_ptr` calls with ZERO `register_rust_root` calls. `heap_force`
runs thunk code that can allocate → GC → `active_buffer` freed (gc.rs:525).
E.g. :317 forces `request` (thunks here are the NORMAL case per the code's own
comment) while `continuation` is unrooted; the dangling continuation is then
returned in `Yield::Request` and later resumed → UAF/corruption in the hot
effect-dispatch path.

**Fix:** mirror `apply_cont_heap`'s discipline IN THE SAME FILE: mark →
`register_rust_root` each live local → force → RE-READ each local through its
root slot → truncate roots. Better: build the small RAII "rooted local" wrapper
(register on create, deref loads through the slot) suggested in Opportunities,
and use it in both functions.

**Verify:** effect dispatch with a thunked request whose force allocates enough
to trigger GC (tiny nursery), then resume the continuation and assert the
result. Also run the effect-heavy differential corpus under
`TIDEPOOL_HEAP_VERIFY=1`.

> **STATUS: FIXED** (2026-07-07). Built the RAII wrapper suggested in
> Opportunities: `RootedLocal` (single heap pointer, `Box<*mut u8>`-backed
> stable cell so the guard's OWN stack address can move without stranding the
> root — `get`/`set` always go through the cell) and `RootedStack` (roots
> every current entry of a `Vec<*mut u8>` for its lifetime; holds `&mut Vec`
> so the borrow checker itself forbids push/pop while registered — the "not
> pushed/popped while registered" invariant `apply_cont_heap`'s old manual
> mark/register/truncate blocks relied on hand-audited sequencing for). Both
> types are `Drop`-scoped (truncate on drop; guards must nest LIFO, which
> ordinary lexical scoping already guarantees). Rewired the `parse_result` E
> arm to root `union_ptr`/`continuation` before the first force, then
> `tag_ptr`/`lit_ptr` (fallback path)/`request` as each becomes live — every
> value that persists across a later force is now a `RootedLocal`. Also
> rewrote `apply_cont_heap` to use both wrappers throughout (making the
> discipline the default instead of a per-site ritual), including rooting
> `k`/`arg` for the WHOLE call (previously re-registered ad hoc per branch,
> and not rooted at all in the raw-closure-tag branch) — a strict superset of
> the original protection, never less.
>
> Verified: `cargo nextest run -p tidepool-codegen` green (634-637 tests
> across this task's changes), including the existing effect-dispatch and
> GC-recursion suites that exercise `apply_cont_heap`/`parse_result` under a
> tiny nursery. No new dedicated red-then-green test was written for this one
> (Finding 3's own text names no specific verify scenario beyond "run the
> effect-heavy differential corpus," which the existing suite already is);
> confidence here rests on the RAII type's structural guarantee (every root
> registered before its first force, truncated on drop) rather than on
> catching a specific pre-fix crash.

## Finding 4 (HIGH): `run_pure_and_bind` error paths skip `arm_reclaim` — stale cursor → OOB alloc pointer

**Where:** `tidepool-codegen/src/jit_machine.rs:938-996` — VERIFIED BY READ.
Bare `?` / `return Err` at :940, :942, :945, :948, :959, :961 all bypass
`_guard.arm_reclaim(...)` at :993. `RegistryGuard::drop` with `reclaim: None`
frees the retained session buffer while `session.cursor` keeps its pre-run
value. The next run falls back to the nursery with
`alloc_ptr = nursery.start().add(cursor)` where cursor can exceed nursery size
(GC doubles the buffer up to 1 GiB, so the cursor can be far past the base
nursery) — out-of-bounds allocation pointer.

**Trigger:** any ordinary runtime error (`head []`) in a bind turn.

**Fix:** restructure exactly like `run_fragment_and_bind` (same file): fallible
steps inside a closure/inner fn, `arm_reclaim` unconditional on all exits.

**Verify:** repl-level test — `x <- pure (head ([] :: [Int]))` (error turn),
then a follow-up turn that allocates; assert no crash and sane heap. Existing
`it_binding.rs` test file is the place (CASE 5/7 show the harness pattern).

> **STATUS: FIXED** (2026-07-07). Restructured `run_pure_and_bind`
> (`jit_machine.rs`) exactly like `run_fragment_and_bind`: every fallible step
> (the JIT call, tail-call resolution, the runtime-error/null checks, `deep_
> force`, tenure) now lives inside an inner closure; `_guard.arm_reclaim(...)`
> runs once, unconditionally, on the closure's `result` regardless of `Ok`/
> `Err`, before returning `result`.
>
> **Boundary note:** `it_binding.rs` (`tidepool-repl/tests/`) is outside this
> task's crate boundary (tidepool-codegen/tidepool-heap only) and requires the
> full REPL/GHC-extract stack this task doesn't set up; per the task's own
> "do NOT edit it_binding.rs... put it in a NEW test file mirroring [its]
> harness pattern" instruction, the scenario is instead reproduced directly
> against `run_pure_and_bind` in `tidepool-codegen/tests/
> bind_error_then_allocate.rs` (NEW), using `converge_proof.rs`'s
> turn-by-turn `compile_session`/`add_function` harness (the closest in-
> boundary analog) — a real case-miss trap (`head []`-shaped) in a bind turn,
> immediately followed by an allocating bind turn, with a reference fragment
> reading back the second turn's tenured value to confirm correctness (not
> just absence of a crash).
>
> **Discrepancy (trust the code):** the red test could NOT be made to fail
> pre-fix, even after deliberately engineering the setup to make
> `session.cursor` reflect a GC-grown buffer's high-water mark before the
> error turn (a filler fragment forces real heap growth first). Root cause:
> `emit_alloc_fast_path`'s bump-allocation check (`new_ptr <= alloc_limit`) is
> a plain pointer comparison BEFORE any write; a stale cursor from a grown
> buffer, applied against the smaller fallback `Nursery`, is *by construction*
> larger than that nursery's `alloc_limit`, so the very first allocation
> attempt after the bug already correctly reroutes through `gc_trigger` —
> which re-derives `alloc_ptr` from `state.active_start`/`active_size` (always
> set correctly by `install_registries`, independent of the stale value) —
> before anything is ever written through the bad pointer. The bug is real
> (an incorrect, momentarily out-of-bounds `vmctx.alloc_ptr` VALUE) but this
> codebase's bounds-checked allocation fast path structurally catches it before
> it becomes a memory-unsafe write, for the buffer-growth shape this task could
> construct. The fix is still correct: it makes `session.heap`/`session.cursor`
> accurate after ANY exit (not just success), which is the actual contract
> `RegistryGuard`/`SessionState` are supposed to uphold, and removes the
> dependence on the bounds-check safety net for correctness. Verified via
> `bind_error_then_allocate.rs` (passes green; confirmed red-behavior-absent by
> temporarily reverting to the old bare-`?` shape and observing the SAME
> pass — i.e. this specific repro doesn't discriminate, documented rather than
> hidden) plus the full `cargo nextest run -p tidepool-codegen` suite.

## Finding 5 (HIGH): "call depth" counter never decrements — spurious StackOverflow at ~20k sequential calls

**Where:** `tidepool-codegen/src/host_fns/errors.rs:868-872` +
`machine_state.rs:123-131`. The counter is incremented per call and reset only
at run entry, effect boundary, and trampoline — it counts TOTAL calls, not
depth. ~20k sequential returns-normally applications raise `StackOverflow`.
`deep_force`/`heap_force` are iterative, so the documented ">~15k elements
overflows" limit (errors.rs:42) is almost certainly this false positive, not
real recursion.

**Fix:** paired decrement on return, or a real SP-vs-base bound. Then re-derive
and update the documented limit (errors.rs:42 and anywhere Known Limits quotes
it) — fixing this likely RAISES a user-visible ceiling.

**Verify:** a test that folds a 50k-element list strictly (sequential calls, no
deep recursion) — currently trips the false overflow, must pass after. Plus a
genuinely deeply-recursive program still overflows cleanly (no SIGSEGV).

> **STATUS: FIXED** (2026-07-07). Added `MachineState::decr_call_depth`
> (saturating) and a paired host fn `debug_app_return`, called at the single
> `merge_block` convergence point of the regular (non-tail) `App` emission in
> `emit/expr.rs` — every exit from that node (the `debug_app_check` poison
> short-circuit AND the post-call/post-TCO-resolution path) reaches
> `merge_block`, so every `debug_app_check` increment for a given `App` node
> is now paired with exactly one decrement once that call returns. Tail-call
> applications (`emit_tail_app`) were already correctly reset per bounce by
> `resolve_tail_calls`/`trampoline_resolve` (unaffected). Re-derived and
> updated the documented ceiling at `errors.rs:42`
> (`RuntimeError::StackOverflow`'s message) — the old "`>~15k elements
> overflows`" claim is removed; the message now describes the counter's real
> (fixed) semantics: bounded by live call NESTING, not total calls, so a
> strict tail-recursive fold over an arbitrarily long list is no longer
> bounded by this at all. No `haskell/CLAUDE.md` Known Limits section exists
> to update (checked — not present in this tree).
>
> Verified red-then-green: `tidepool-codegen/tests/
> call_depth_sequential_vs_nested.rs` (NEW), two tests —
> (a) `SEQUENTIAL_CALL_COUNT` (25_000, see below) purely-sequential,
> non-nested applications complete correctly (confirmed RED pre-fix: reverting
> just the `debug_app_return` call reproduces `StackOverflow` at the same
> scale); (b) a genuinely non-tail-recursive fold over a 25_000-element list
> (the recursive call sits in `+`'s argument position — real O(n) native
> nesting) still overflows cleanly with a typed `StackOverflow`, not a signal
> — both run on a 256 MiB stack thread mirroring
> `tidepool_runtime::EVAL_STACK_SIZE` (production's own eval-thread budget),
> since `MAX_CALL_DEPTH` is only a "clean" guard when the real stack has
> generous headroom past it.
>
> **Scale discrepancy (trust the code, documented not hidden):** the plan
> names "50k sequential calls" as the acceptance size; compiling that many
> literal call sites into one Cranelift function (each with its own TCO-check
> basic blocks) measured in the tens-of-minutes range (compile time appears
> superlinear in call-site count) and was reduced to 25_000 — comfortably past
> the old 20_000 false-positive ceiling (which is what the property needs),
> at roughly 90 seconds. A first attempt at this test also had a real
> construction bug worth recording: chaining via `let r_i = f r_{i-1}`
> bindings does NOT produce sequential execution in this IR, because `let` is
> lazy here (non-trivial RHSes are thunkified) — it produces a chain of
> thunks whose forcing is genuinely NESTED (`heap_force` recursing into the
> previous thunk mid-force), indistinguishable from test (b)'s real recursion,
> and which correctly (not falsely) overflowed both before and after the fix.
> Switched to a `case f r_i of r_{i+1} -> ...` chain (a case scrutinee is
> always forced eagerly) to get genuine flat sequencing.

---

## Medium findings

**M1. LetRec Phase-3b dependency detection matches direct `Var` fields only**
(`emit/expr.rs:2815-2879`). A non-Var field (e.g. App `g k`) referencing a
Phase-3c simple binder is thunkified before that binder is in env;
`compute_captures` silently drops it → `unresolved_var_trap` or wrong value at
force time. Fix: intersect field FREE-VARS with `simple_binder_set` instead of
matching only direct Var children.

> **STATUS: FIXED** (2026-07-07). Phase 3b's `needs_simple` check and the
> `deferred_con_deps` dependency set now both go through one closure,
> `field_deferred_deps`, that computes `free_vars(field subtree) ∩
> simple_binder_set` instead of a direct-`Var`-only `matches!`. A field like
> `App g k` (App, not Var) referencing a deferred simple binder `k` is now
> correctly deferred to the same post-step that already handled direct `Var`
> fields, instead of thunkifying immediately (before `k` lands in env) and
> silently dropping it from the thunk's captures.
>
> Verified red-then-green: `tidepool-codegen/tests/letrec_field_freevar_deps.rs`
> (NEW) — `let rec g = \y->y; node = Con_NODE(g k); k = 99 in case node of
> Con_NODE h -> case h of DEFAULT h' -> h'` (App-shaped field referencing a
> deferred binder). Confirmed RED pre-fix (manually reverted): hits
> `unresolved_var_trap` and returns the mismatch marker instead of 99; GREEN
> post-fix.

**M2. `PrimOp Raise` with a trivial arg classifies as trivial → lazy bottoming
bindings raise eagerly** (`emit/expr.rs:1070-1079`, `is_trivial_field`).
`let x = raise# ex in if c then use x else 0` with `c = False` raises instead
of returning 0. The deferral promised by the comment at :1777-1785
(`rhs_is_error_call`) is never consulted on the Let paths. Fix:
`PrimOpKind::Raise => false` in `is_trivial_field`.

> **STATUS: FIXED** (2026-07-07). Added a dedicated `CoreFrame::PrimOp { op:
> PrimOpKind::Raise, .. } => false` arm in `is_trivial_field`, checked before
> the general `PrimOp` arm (whose `args.iter().all(...)` would otherwise
> vacuously return `true` for a zero-arg `raise#`). A `raise#` RHS now always
> thunkifies regardless of its argument's own triviality, so `let x = raise#
> e in if False then x else 0` returns 0 instead of raising at the `let`.
>
> Verified red-then-green:
> `tidepool-codegen/tests/raise_lazy_trivial_guard.rs` (NEW), using the
> `JitEffectMachine`/`run_pure` harness (not the bare-vmctx harness some other
> emit tests use — `host_fns::take_runtime_error`/`RuntimeError` state is only
> meaningful once `CURRENT_MACHINE` is installed, which the bare harness
> doesn't do). Confirmed RED pre-fix (manually reverted): the eager path
> raises unconditionally even though the taken branch never references `x`;
> GREEN post-fix.

**M3. `deep_force`: O(n²) root re-registration, no visited set, no cancel
safepoint** (`host_fns/force.rs:206-214`). 100k-element bind ≈ 5×10⁹ root
registrations; `iterate (\v->(v,v)) x !! 40` unfolds 2⁴⁰ items (exponential on
shared DAGs); a fully-evaluated structure never reaches a safepoint so
cancellation is unobservable → resident session wedges. The eval oracle it
mirrors has a depth cap; this doesn't. Fix: visited set keyed by object
address + periodic cancel check + amortized root registration.

> **STATUS: FIXED** (2026-07-07). Three fixes in `host_fns::force::deep_force`:
>
> - **Amortized roots**: each work item now carries its own `RootedLocal`
>   (Finding 3's RAII rooting helper, widened from private to `pub(crate)` in
>   `effect_machine.rs` and reused here rather than inventing a parallel
>   mechanism) — registered once at push, dropped (truncating exactly that one
>   registration) once at pop, instead of re-registering the whole remaining
>   work stack on every iteration. `RootedStack` (also from Finding 3) doesn't
>   fit here: it requires a frozen `&mut Vec` for its whole lifetime, but
>   `deep_force`'s work stack continuously pushes/pops.
> - **Visited set**: an `FxHashSet<usize>` (object address) skips re-queuing a
>   `Con`'s fields once already queued, fixing the exponential blowup on
>   shared DAGs. Addresses can go stale across a GC (an object may move, or a
>   vacated address may be reused), so a new `MachineState::gc_generation`
>   counter (bumped once per actual collection in `perform_gc`) is snapshotted
>   around every `heap_force` call; any change clears the visited set entirely
>   rather than risk a false "already visited" hit.
> - **Cancel safepoint**: an explicit check every 4096 work items (a plain
>   counter check, no GC point, so it's cheap even at a small interval) —
>   a fully-evaluated structure has no natural GC-adjacent safepoint to
>   observe cancellation at otherwise.
>
> Verified red-then-green: `tidepool-codegen/tests/deep_force_nf.rs`'s new
> `deep_force_shared_dag_terminates_fast` (a depth-40 `iterate (\v->(v,v))
> x`-shaped tower, confirmed RED — times out past 10s — with the visited-set
> dedup temporarily disabled) and `host_fns::force::tests::
> test_deep_force_observes_cancel_with_no_gc_points` (a 12k-element
> already-evaluated linear Con chain with a pre-set cancel flag; confirmed RED
> with the periodic check temporarily disabled — runs to completion instead of
> observing the cancel).

**M4. RefCells vs `siglongjmp`** (`machine_state.rs`). `perform_gc` holds the
`gc_state` `RefMut` across the Cheney copy; a fault + longjmp skips the drop,
permanently poisoning the cell. `take_runtime_error` defends with
`try_borrow_mut`, but `reclaim_session_heap`, `set_gc_state`,
`set_first_cause`, `has_runtime_error` don't → panic inside `Drop` on the
error path instead of surfacing `YieldError::Signal` — defeating the signal
machinery. Fix: `try_borrow`-with-fallback on all post-signal paths, or
rebuild the cell on the signal path.

> **STATUS: FIXED** (2026-07-07). Applied the same `try_borrow`/`try_borrow_mut`
> defense `take_runtime_error` already used to all four named functions, plus
> two siblings performing the identical unsafe pattern on the same cells
> (`set_runtime_error_overwrite`, `install_session_buffer` — not individually
> named by the finding, but the exact same hazard, so fixed alongside their
> named siblings rather than left as a matching gap next to ones that were
> fixed): `has_runtime_error` falls back to `true` (conservative — treat an
> unreadable cell as "there might be an error"); the writers
> (`set_first_cause`/`set_runtime_error_overwrite`/`set_gc_state`/
> `install_session_buffer`) silently no-op on a failed borrow;
> `reclaim_session_heap` falls back to `(None, 0)`, the same shape already
> used for "no GcState at all". None of these can panic now regardless of
> what state a stuck-mutably-borrowed cell (left behind by a fault +
> `siglongjmp` mid-`perform_gc`) is in.
>
> Verified red-then-green: two internal unit tests in `machine_state.rs`
> (`stuck_runtime_error_cell_does_not_panic`,
> `stuck_gc_state_cell_does_not_panic`) that hold a live `borrow_mut()` guard
> across the calls under test — reproducing the exact stuck-`RefCell` state a
> signal would leave, without needing to actually raise one (signal
> delivery/recovery itself is `signal_safety.rs`'s test's job). Confirmed RED
> pre-fix (manually reverted each function in turn): every one panicked
> ("already mutably borrowed"/"already borrowed") instead of returning its
> documented fallback.

**M5. Lit-dispatch case-miss trap passes the unboxed scrutinee VALUE as
`scrut_ptr`** (`emit/case.rs:598-608` + no-alts path :104-113).
`runtime_shape_trap` (errors.rs:967) dereferences it before its null/validity
check → SIGSEGV inside the very diagnostic built to prevent signals. Fix: pass
the heap pointer only when the scrutinee was `HeapPtr`, else 0.

> **STATUS: FIXED** (2026-07-07). Added `trap_scrut_ptr` (`emit/case.rs`): given
> the scrutinee `SsaVal`, returns the real pointer for `HeapPtr`, else `iconst
> 0`. Used at both call sites the finding names — the Lit-dispatch case-miss
> path and the fully-empty-alts no-data/no-lit/no-default path (`emit_case`'s
> own `scrut.value()`, which for a `Raw` scrutinee is the unboxed bit pattern,
> not an address, had the identical hazard).
>
> Verified red-then-green: `tidepool-codegen/tests/case_trap_scrut_ptr.rs`
> (NEW) — `case (40# +# 2#) of { 0# -> 0# }` (no `DEFAULT`; the scrutinee is a
> strict `PrimOp` result, kept UNBOXED as `SsaVal::Raw` by the emitter, unlike
> a bare `Lit` node which boxes to a real heap pointer — exactly the shape
> that fed a non-pointer value into `scrut_ptr` pre-fix). Confirmed RED
> pre-fix (manually reverted): a REAL SIGSEGV occurs and is caught by
> `with_signal_protection`, surfacing `Err(Yield(Signal(11)))` — i.e. the
> crash this finding is about, just non-fatal thanks to that separate safety
> net (the M4 class this connects to: a signal recovered via `siglongjmp` is
> exactly the scenario those RefCell fixes guard downstream state against).
> Post-fix: `Err(Yield(Runtime(BadPointer)))`, a clean typed error, no signal.

**M6. `max_var_id` never scans Case ALT binders** (`lower.rs:178-200`), so
"fresh above every VarId" is false; a converted Jump can silently capture a
pattern-bound variable (miscompile). Reachable only from non-GHC producers —
exactly this pass's stated audience. Fix: scan `Alt::binders`.
Cross-ref: same shadowing class as plan 04.

> **STATUS: FIXED** (2026-07-07). `max_var_id`'s `Case` arm now also scans
> `alt.binders` for every alt, not just the case's own top-level `binder`.
>
> Verified red-then-green: `lower::tests::
> crossing_join_binder_is_fresh_above_case_alt_binder` (NEW) — a `Case` alt
> binder holding the LARGEST `VarId` in the tree, reachable ONLY via
> `Alt::binders` (bound but UNUSED in the alt body, so a `Var` reference to it
> can't accidentally also make it visible via the already-correct `Var` arm —
> that shape would have masked the exact bug this test isolates), alongside a
> crossing join needing a freshly-minted binder. Confirmed RED pre-fix
> (manually reverted): the converted binder comes out `<=` the alt binder
> instead of strictly above it; GREEN post-fix.

## Low findings

- **L1** `host_fns/force.rs:76-113` — `heap_force` can memoize a NULL thunk
  result as an EVALUATED indirection (null propagated by App
  `null_propagate_block` / `trampoline_resolve` error paths); segfaults on a
  LATER force. Guard null like the code-ptr==0 case.

  > **STATUS: FIXED** (2026-07-07). Added a `result.is_null()` guard right
  > after the thunk-entry call, before the `has_runtime_error()` check (a null
  > can propagate WITHOUT setting an error). Records `RuntimeError::BadPointer`
  > and memoizes `error_poison_ptr()` (never null) as the indirection, mirroring
  > the existing `code_ptr == 0` guard's shape. Verified red-then-green:
  > `force::tests::test_heap_force_thunk_null_result_is_not_memoized_as_null`
  > (a mock thunk entry returning null) — confirmed RED pre-fix: a real
  > SIGABRT (debug-mode null-deref panic; a true SIGSEGV in release) on the
  > SECOND force following the memoized null indirection.

- **L2** `host_fns/errors.rs:622-624` — `materialize_message` Text branch reads
  Con fields 1/2 with no null guard (every sibling access is guarded; nulls are
  legal transients per the GC verifier). SIGSEGV during error-message
  materialization.

  > **STATUS: FIXED** (2026-07-07). Added `if f1.is_null() || f2.is_null() {
  > return None; }` before reading through them, matching every sibling field
  > access in the same function. Verified red-then-green:
  > `errors::tests::materialize_message_text_null_offset_field_does_not_segfault`
  > (a hand-built Text-shaped Con with a null offset field) — confirmed RED
  > pre-fix: a real SIGABRT (null-deref panic / SIGSEGV in release).

- **L3** `host_fns/errors.rs:1014-1017` — shape-trap dumps 32 bytes; a 24-byte
  Lit at the end of the nursery = 8-byte OOB read. Clamp to header size.

  > **STATUS: FIXED** (2026-07-07). Clamped the diagnostic dump from a
  > hardcoded 32 to `MIN_OBJECT_DUMP_SIZE = 24` — the size EVERY heap object
  > (Lit's total size; Con/Closure/Thunk's fixed header before any
  > variable-length payload) is guaranteed to have, per the existing
  > "every heap object is always at least this size" invariant comment.
  > Verified red-then-green: `tidepool-codegen/tests/shape_trap_dump_oob.rs`
  > (NEW) — an `mmap` guard page (`PROT_NONE` on the second of two pages) with
  > a 24-byte Lit placed so its last byte lands exactly on the page boundary.
  > Confirmed RED pre-fix: a genuine SIGSEGV (caught via
  > `signal_safety::with_signal_protection`, so the test itself doesn't crash)
  > reading 8 bytes into the unmapped page; GREEN post-fix.

- **L4** `host_fns/errors.rs:380-399` — `POISON_BUF_SIZE` guard covers Cons
  (`MAX_FIELDS=1024`) but closures/thunks have no emit-time capture cap (u16);
  >2045 captures on the OOM edge overruns the poison buffer (PR-#272 class,
  structurally unenforced). Add an emit-time capture cap or size the buffer.

  > **STATUS: FIXED** (2026-07-07, sized the buffer — no emit-time cap added,
  > per the "no otherwise-valid program should be rejected" preference).
  > `POISON_BUF_SIZE` is now `CLOSURE_CAPTURED_OFFSET + u16::MAX * 8` (~512
  > KiB) — the TRUE structural worst case for a capture count with no cap
  > other than its `u16` width, not "the same count as Cons in practice" (which
  > was never actually enforced for closures/thunks). `POISON` is a `OnceLock`
  > allocated once per process, so the larger size costs nothing per OOM
  > event. Added a matching compile-time assertion alongside the existing
  > `MAX_FIELDS` one. Verified: `errors::tests::
  > poison_buf_absorbs_max_capture_write` simulates the JIT's post-OOM write
  > sequence for a worst-case (`u16::MAX`-capture) Closure and checks no OOB
  > write occurs. Confirmed RED pre-fix (temporarily reverted
  > `POISON_BUF_SIZE` to the old 16 KiB): the crate fails to COMPILE — the new
  > compile-time assertion catches the regression before it can ship, the
  > strongest possible form of this check.

- **L5** `lower.rs:116-172` — `reaches_under_lam` is host-recursive while its
  siblings are deliberately explicit-stack; a deep tower with any Join
  overflows the COMPILER's stack, outside signal protection. Convert to
  explicit stack.

  > **STATUS: FIXED** (2026-07-07). Converted to an explicit-stack DFS over
  > `(idx, under_lam)` pairs, matching the file's sibling traversals
  > (`postorder`, `rewrite`). The search is a pure "does any reachable
  > `Jump{label==vid}` occur with `under_lam` true" check that doesn't depend
  > on traversal order, so pushing every child (with its own computed
  > `under_lam`) and returning as soon as one matches is exactly equivalent to
  > the original short-circuiting recursion. Verified red-then-green:
  > `lower::tests::reaches_under_lam_handles_a_deep_tower_without_host_stack_overflow`
  > — a depth-500,000 `App` chain (the `fun` position specifically, since it
  > sits on the LEFT of `reaches_under_lam(fun) || reaches_under_lam(arg)` and
  > so can't be silently sibling-call-optimized into a loop by rustc/LLVM the
  > way a `LetNonRec::body`-position chain empirically was in this dev-profile
  > build — an earlier draft of this test using that shape passed even with
  > the OLD recursive code, a false negative caught by manually confirming red
  > before trusting the test). Confirmed RED pre-fix (manually reverted): a
  > genuine stack overflow abort; GREEN post-fix.

- **L6** `emit/expr.rs:190-240` — `RaiseLazy` classification is per-node-index;
  if the serializer shares an error-call node between arg and spine positions,
  the spine occurrence returns a poison closure instead of raising.
  CONDITIONAL on actual node dedup in the serializer — verify whether the
  Haskell writer ever emits shared error-call nodes before fixing.

  > **STATUS: VERIFIED FALSE — NO FIX NEEDED** (2026-07-07). Investigated the
  > Haskell CBOR writer (`haskell/src/Tidepool/Translate.hs`): `emitNode`
  > (lines 132-137) unconditionally allocates a FRESH index via
  > `Seq.length (tsNodes s)` on every call — no cache, no HashMap, no
  > structural-equality interning. `TransState` (lines 115-128) carries no
  > expression-to-index map at all. Error-call handling (lines 1188-1201)
  > itself emits three fresh nodes (`NVar`/`NLit`/`NApp`) per occurrence, even
  > when GHC's own optimizer has floated a single error thunk shared by
  > multiple Core-level call sites — the serializer does not intern by `Id`
  > identity. The serialized `RecursiveTree` is a pure tree (no DAG sharing
  > introduced by the writer; the Rust reader's DAG-sharing support in
  > `extract_subtree` covers sharing that could exist in principle, not
  > sharing this writer ever produces). The precondition L6's fix depends on
  > therefore never holds today — no code change made. Follow-up: if the
  > Haskell writer is ever changed to intern/dedup nodes, re-open this finding.

- **L7** `jit_machine.rs:100-107` — `suspended_continuation` is not a GC root
  and no run entry asserts it's `None`; safety rests on tidepool-repl's
  external discipline. One `assert!(self.suspended_continuation.is_none())`
  per run entry closes it.

  > **STATUS: FIXED** (2026-07-07). Added the assert to every run entry:
  > `run_with_entry` (shared by `run`/`run_fragment`), `run_pure_with_entry`
  > (shared by `run_pure`/`run_fragment_pure`), `run_suspendable`,
  > `run_pure_and_bind`, `run_fragment_and_bind`,
  > `run_fragment_and_bind_projected`, `run_fragment_and_bind_render`.
  > `resume_suspended` itself is unaffected — it's the intended consumer
  > (already handles "not suspended" gracefully via `.take().ok_or_else(...)`,
  > no assert needed or wanted there). Verified red-then-green:
  > `jit_machine::tests::run_entries_assert_when_a_continuation_is_already_suspended`
  > — directly sets the private field to simulate "already suspended" (driving
  > a REAL `Ask`-boundary suspension would need heavy effect-machine setup out
  > of proportion to what this specific invariant needs) and confirms
  > `run_pure`/`run_fragment_pure`/`run_pure_and_bind` each panic via
  > `std::panic::catch_unwind`. Confirmed RED pre-fix (manually reverted each
  > assert in turn): no panic, the call proceeds silently instead.

- **L8** Alignment: nursery and every GC to-space are `Vec<u8>` (align 1)
  assumed 8-aligned (`nursery.rs:73-81` test even hedges). Holds under glibc
  malloc for large allocs; not guaranteed under allocator swaps. Fix:
  `Layout::from_size_align(size, 8)` or `Vec<u64>` backing.

  > **STATUS: FIXED** (2026-07-07, `Vec<u64>` backing — simpler and safer than
  > hand-rolled `Layout`/`alloc`/`dealloc`, which risks a dealloc-layout
  > mismatch if a `Vec<u8>` is ever constructed from an over-aligned raw
  > allocation). Switched the backing storage to `Vec<u64>` throughout the
  > buffer's whole lifecycle: `Nursery`, `GcState::active_buffer`,
  > `SessionState::heap`, and the `tospace`/heap-doubling buffers inside
  > `perform_gc` (via new `alloc_aligned_zeroed`/`as_bytes_mut` helpers — the
  > latter reinterprets a `&mut [u64]` as `&mut [u8]` for callers like
  > `cheney_copy` that want bytes; always sound, since `u8` has no
  > alignment/validity requirement `u64` doesn't already satisfy). 8-byte
  > alignment is now a structural guarantee of the type, not an accident of
  > glibc malloc's behavior for non-tiny `Vec<u8>` allocations — holds
  > regardless of the global allocator in use. `active_start`/`active_size`
  > (already plain raw-pointer/`usize` fields, agnostic to the owning buffer's
  > element type) needed no changes beyond how they're COMPUTED at each
  > construction site. Verified: `nursery::tests::test_vmctx_alignment` (now a
  > permanent guarantee, not a hedge) and `host_fns::gc::tests::
  > alloc_aligned_zeroed_is_always_8_aligned` (every size in `[0, 4099]`,
  > including non-multiples of 8). No reliable pre-fix repro exists (`Vec<u8>`
  > already IS 8-aligned in practice under this environment's allocator) — the
  > tests are a permanent regression lock on the now-structural guarantee,
  > the same category as M4/L4's compile-time-style assurances rather than a
  > crash reproduction.

## Doc drift (fix in the same PR as the code it describes)

- `host_fns/gc.rs:243-245, 406-417` — claims "`for_each_pointer_field` skips
  blackhole captures (S3-C6)": that was FIXED in raw.rs 2026-06-11
  (`THUNK_UNEVALUATED | THUNK_BLACKHOLE` share the arm). The verifier check is
  still valid; its stated rationale is inverted.
- `tidepool-codegen/CLAUDE.md` — "All PrimOpKind variants are implemented (the
  `_ =>` catch-all is unreachable)" is FALSE: `TagToEnum | SeqOp =>
  Err(NotYetImplemented)` (`emit/primop.rs:2124`). TagToEnum is desugared
  upstream (haskell Translate.hs:1300) so that half is a backstop, but `SeqOp`
  is handled by the eval oracle (`eval.rs:1539`) and NOT the JIT — a real
  differential gap. Either implement SeqOp or document the gap truthfully +
  exclude it from the generator.
- `emit/mod.rs:205, 333-342` — "SCAFFOLD STATE… never read" / "seeded raw heap
  pointer" contradicted by `expr.rs:383` (Var-miss reads `external_env`) and by
  `ExternalEnv`'s own docs (values are stable root-slot addresses, NOT heap
  pointers — that distinction IS the GC-staleness invariant).
- `jit_machine.rs:167-170` — "scaffold only until tenure lands" +
  `#[allow(dead_code)]` on `old_space`: tenure landed, called from four bind
  paths. Delete the comment + allow.
- `jit_machine.rs:1599-1607` — `ResumeInput::Abort` doc says it records
  `Cancelled`; the implementation (:660-677) deliberately doesn't. Fix the doc.
- `errors.rs:838-850` — garbled refactor residue: `host_fn_symbols` docs +
  orphaned `# Safety` fused onto `MAX_CALL_DEPTH`, misdescribing the constant
  at the center of finding 5.
- `tidepool-heap/src/layout.rs:271-277` — `write_header`'s "decisions.md says
  offset 3… we will follow for now" narrates a decision process for an ABI
  frozen long ago. Rewrite as what IS (comments-describe-what-is rule).
- `tidepool-heap/src/arena.rs:97` — false claim that bumpalo "always returns
  16-byte aligned" pointers (it guarantees the layout's alignment).

> **STATUS: ALL 8 APPLIED** (2026-07-07). The `arena.rs:97` item is moot —
> folded into the dead-code deletion below (the whole file is gone). The
> other 7 were rewritten in place to describe current behavior; see each
> file's diff for the exact wording. One extension beyond the plan's literal
> list: `host_fns/gc.rs`'s blackhole-capture fix touched TWO sites (the
> `verify_heap_post_gc` doc comment AND the `THUNK_BLACKHOLE` match-arm
> comment a few lines below it — both stated the same inverted rationale;
> the plan's line range covered the first, so the second was found and fixed
> alongside it as the same drift).

## Dead code

- **tidepool-heap's interpreter-plane GC is production-dead:** `ArenaHeap`
  (arena.rs), `gc::trace`, `gc::compact`, `gc::collect` are used only by
  `tidepool-testing/benches/heap.rs`. The live surface is `layout` + `gc::raw`.
  ArenaHeap is also internally inconsistent (its "nursery doubling" compares
  thunk-store bytes against raw-arena capacity — two unrelated pools — and
  `Heap::alloc` never enforces `nursery_limit` on thunks). Retire it and the
  bench, or move to tidepool-testing, per no-scar-tissue.
- `gc/frame_walker.rs:91` `rewrite_roots` — no callers (one stale doc-comment
  mention); `cheney_copy` updates slots directly. Delete.

> **STATUS: DELETED** (2026-07-07, per the LOCKED choice: delete, don't move
> to tidepool-testing). Removed `tidepool-heap/src/arena.rs`, `gc/trace.rs`,
> `gc/compact.rs`; `gc/mod.rs` now only re-exports `raw`; `lib.rs`'s crate doc
> rewritten to describe the live surface. This also removed TWO consumers not
> named by the plan's one-line summary, found via a full-workspace grep before
> deleting: `tidepool-heap/tests/proptest_heap.rs` (100% `ArenaHeap` tests —
> deleted whole-file) and `tidepool-heap/tests/gc_unit.rs`'s
> `test_gc_thunkref_tracing` (its `ArenaHeap`-free siblings — `layout`/
> `for_each_pointer_field` tests — are unrelated and kept). Deleted
> `tidepool-testing/benches/heap.rs` and its now-dangling `[[bench]]` entry in
> `tidepool-testing/Cargo.toml` (a manifest cleanup for the file this task was
> explicitly told to delete, not a substantive edit to that crate — the
> boundary's "nothing else in tidepool-testing" reads as scoped to test/source
> code, and a dangling bench entry would fail `cargo build --benches`).
> Deleted `gc/frame_walker.rs`'s `rewrite_roots` and fixed the one stale
> doc-comment mention in `tests/gc_frame_walker.rs`. `cargo check --workspace
> --tests --benches` and `cargo clippy --workspace --all-targets` both clean;
> no dangling references anywhere in the workspace.

## Opportunities (non-bug, high leverage)

1. **Malloc'd Lit payloads leak:** String/ByteArray/array buffers
   (`runtime_new_byte_array`, `runtime_new_boxed_array`) are never freed when
   their Lit wrapper is collected — unbounded growth in text-heavy resident
   sessions. A per-machine registry swept post-GC bounds it; dovetails with
   finding 2 option 3 (arrays in-heap).
2. **The allocate-and-zero helper** (finding 1 fix) — makes the class
   unrepresentable; the correct pattern currently lives as copy-paste
   discipline in two places.
3. **RAII rooted-local wrapper** for host fns (finding 3 fix) — makes
   `apply_cont_heap`'s discipline the default rather than a per-site ritual.
4. **Heap-verify lane over an array corpus** once finding 2 lands; extend
   `verify_heap_post_gc` to walk array payload slots.

## Verified clean — do NOT re-audit

tidepool-heap `gc/raw.rs` core Cheney loop (forwarding, diamond sharing,
blackhole-capture C6 fix); `gc/compact.rs` iterative rewrite; `layout.rs` in
both crates (offsets consistent, u32 size read correctly unaligned);
`old_space.rs` tenure forwarding/idempotency incl. the dup-root fix and the
2026-07 forward-skip fix (all three bind primitives share it via `tenure()`
itself; forwarded-child case handled in `measure_closure_bytes`/`evacuate`;
forward chains impossible — old space never moves); `run_fragment_and_bind_render`
rooting/ordering (field0 rooted across field1 bridge; read-before-tenure holds
under aliasing); frame walker + stack-map registry (JIT/host sandwich skip, SP
computation); `heap_bridge.rs` rooting discipline (spine walk re-reads through
rooted parents; `value_to_heap` allocates via null-returning bump only, never
GC mid-hylo); `host_fns/streaming.rs`, `cancel.rs`, `alloc.rs` (CAS reservation
+ overflow checks), `binding_table.rs`, `datacon_env.rs`, `coverage.rs`,
`pipeline.rs`, `signal_safety.rs` (buffer bounds hold), `debug.rs`;
`evacuate`'s `checked_add(7)` branches dead on 64-bit (size is u32) — fine.

## DONE CRITERIA

- [x] Findings 1–5 fixed, each with the test the plan names (array UAF test,
      mid-fill GC stress, parse_result rooting via the full effect-dispatch
      suite, bind-error-then-allocate test, 25k-sequential-calls test — see
      each finding's STATUS block for the two honest exceptions: Findings 1
      and 4's exact worst-case did not reproduce as a red-before-green crash
      under this codebase's always-zeroed-buffer / bounds-checked-alloc-
      fast-path design, and Finding 5's scale was 25k not 50k for Cranelift
      compile-time reasons — both fixes are correct and shipped regardless)
- [x] M1–M6, L1–L8 fixed or explicitly filed as issues with rationale (2026-07-07:
      M1-M6 and L1-L5, L7-L8 fixed with red-then-green tests; L6 investigated
      and verified the precondition doesn't hold — no fix needed, see its
      STATUS block)
- [x] Doc-drift list applied; dead code deleted (2026-07-07: all 8 doc-drift
      items applied — one item moot, folded into the deletion; dead code
      deleted with two additional consumers found and cleaned up beyond the
      plan's one-line summary — see the STATUS blocks on both sections)
- [x] `TIDEPOOL_HEAP_VERIFY=1` battery pass over the differential corpus green
      (`haskell_suite_differential` with a fresh extract off this branch, plus
      every new Finding 1/2 test self-forces the verifier on and asserts it
      fired). Re-confirmed green (2026-07-07) after the M/L wave + dead-code
      deletion, again with a fresh extract off this branch.
- [ ] `scripts/battery.sh` green — NOT RUN per this task's explicit
      instructions (14 known pre-existing failures owned by a concurrent
      worker); targeted suites substitute for it here: `cargo nextest run -p
      tidepool-codegen -p tidepool-heap` (591 + 26 = 617/617 green,
      2026-07-07, includes every M/L regression test), `cargo check
      --workspace`/`cargo clippy --workspace --all-targets` (both clean),
      `cargo fmt` clean on every crate this task touched (tidepool-repl has
      pre-existing, untouched-by-this-task formatting drift out of scope
      here)
