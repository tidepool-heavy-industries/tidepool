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

---

## Medium findings

**M1. LetRec Phase-3b dependency detection matches direct `Var` fields only**
(`emit/expr.rs:2815-2879`). A non-Var field (e.g. App `g k`) referencing a
Phase-3c simple binder is thunkified before that binder is in env;
`compute_captures` silently drops it → `unresolved_var_trap` or wrong value at
force time. Fix: intersect field FREE-VARS with `simple_binder_set` instead of
matching only direct Var children.

**M2. `PrimOp Raise` with a trivial arg classifies as trivial → lazy bottoming
bindings raise eagerly** (`emit/expr.rs:1070-1079`, `is_trivial_field`).
`let x = raise# ex in if c then use x else 0` with `c = False` raises instead
of returning 0. The deferral promised by the comment at :1777-1785
(`rhs_is_error_call`) is never consulted on the Let paths. Fix:
`PrimOpKind::Raise => false` in `is_trivial_field`.

**M3. `deep_force`: O(n²) root re-registration, no visited set, no cancel
safepoint** (`host_fns/force.rs:206-214`). 100k-element bind ≈ 5×10⁹ root
registrations; `iterate (\v->(v,v)) x !! 40` unfolds 2⁴⁰ items (exponential on
shared DAGs); a fully-evaluated structure never reaches a safepoint so
cancellation is unobservable → resident session wedges. The eval oracle it
mirrors has a depth cap; this doesn't. Fix: visited set keyed by object
address + periodic cancel check + amortized root registration.

**M4. RefCells vs `siglongjmp`** (`machine_state.rs`). `perform_gc` holds the
`gc_state` `RefMut` across the Cheney copy; a fault + longjmp skips the drop,
permanently poisoning the cell. `take_runtime_error` defends with
`try_borrow_mut`, but `reclaim_session_heap`, `set_gc_state`,
`set_first_cause`, `has_runtime_error` don't → panic inside `Drop` on the
error path instead of surfacing `YieldError::Signal` — defeating the signal
machinery. Fix: `try_borrow`-with-fallback on all post-signal paths, or
rebuild the cell on the signal path.

**M5. Lit-dispatch case-miss trap passes the unboxed scrutinee VALUE as
`scrut_ptr`** (`emit/case.rs:598-608` + no-alts path :104-113).
`runtime_shape_trap` (errors.rs:967) dereferences it before its null/validity
check → SIGSEGV inside the very diagnostic built to prevent signals. Fix: pass
the heap pointer only when the scrutinee was `HeapPtr`, else 0.

**M6. `max_var_id` never scans Case ALT binders** (`lower.rs:178-200`), so
"fresh above every VarId" is false; a converted Jump can silently capture a
pattern-bound variable (miscompile). Reachable only from non-GHC producers —
exactly this pass's stated audience. Fix: scan `Alt::binders`.
Cross-ref: same shadowing class as plan 04.

## Low findings

- **L1** `host_fns/force.rs:76-113` — `heap_force` can memoize a NULL thunk
  result as an EVALUATED indirection (null propagated by App
  `null_propagate_block` / `trampoline_resolve` error paths); segfaults on a
  LATER force. Guard null like the code-ptr==0 case.
- **L2** `host_fns/errors.rs:622-624` — `materialize_message` Text branch reads
  Con fields 1/2 with no null guard (every sibling access is guarded; nulls are
  legal transients per the GC verifier). SIGSEGV during error-message
  materialization.
- **L3** `host_fns/errors.rs:1014-1017` — shape-trap dumps 32 bytes; a 24-byte
  Lit at the end of the nursery = 8-byte OOB read. Clamp to header size.
- **L4** `host_fns/errors.rs:380-399` — `POISON_BUF_SIZE` guard covers Cons
  (`MAX_FIELDS=1024`) but closures/thunks have no emit-time capture cap (u16);
  >2045 captures on the OOM edge overruns the poison buffer (PR-#272 class,
  structurally unenforced). Add an emit-time capture cap or size the buffer.
- **L5** `lower.rs:116-172` — `reaches_under_lam` is host-recursive while its
  siblings are deliberately explicit-stack; a deep tower with any Join
  overflows the COMPILER's stack, outside signal protection. Convert to
  explicit stack.
- **L6** `emit/expr.rs:190-240` — `RaiseLazy` classification is per-node-index;
  if the serializer shares an error-call node between arg and spine positions,
  the spine occurrence returns a poison closure instead of raising.
  CONDITIONAL on actual node dedup in the serializer — verify whether the
  Haskell writer ever emits shared error-call nodes before fixing.
- **L7** `jit_machine.rs:100-107` — `suspended_continuation` is not a GC root
  and no run entry asserts it's `None`; safety rests on tidepool-repl's
  external discipline. One `assert!(self.suspended_continuation.is_none())`
  per run entry closes it.
- **L8** Alignment: nursery and every GC to-space are `Vec<u8>` (align 1)
  assumed 8-aligned (`nursery.rs:73-81` test even hedges). Holds under glibc
  malloc for large allocs; not guaranteed under allocator swaps. Fix:
  `Layout::from_size_align(size, 8)` or `Vec<u64>` backing.

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

- [ ] Findings 1–5 fixed with the red tests described (array UAF test, mid-fill
      GC stress, parse_result rooting test, bind-error-then-allocate test,
      50k-sequential-calls test)
- [ ] M1–M6, L1–L8 fixed or explicitly filed as issues with rationale
- [ ] Doc-drift list applied; dead code deleted
- [ ] `TIDEPOOL_HEAP_VERIFY=1` battery pass over the differential corpus green
- [ ] `scripts/battery.sh` green
