# Lazy Effect-Result Materialization (design + implementation notes)

Problem: effect handler responses are eagerly converted Value→heap
(`heap_bridge::value_to_heap`) with a 10k-node cap checked in
`jit_machine.rs` (~line 261, `EffectResponseTooLarge`). Programs that fold a
big result down (`glob ... >>= length`, `sgRuleFind ... >>= length`) die for
intermediate data they never return. Two live specimens in
.tidepool/dogfood-log.md.

## Design: host-materialized lazy list tails

When a list-shaped response exceeds a threshold, return a THUNK whose code
pointer is a HOST function (precedent: poison trampolines; `heap_force`
calls thunk entries blind through a transmute, host fns work). Forcing
materializes the next K elements + a new tail thunk.

- Registry: thread-local `LAZY_RESULTS: RefCell<HashMap<u64, LazyList>>`
  in host_fns; `LazyList { cons_tag: u64, nil_tag: u64, items: Vec<Value>,
  next: usize }`. Values are pure Rust data (no heap ptrs) — GC-inert.
  Cleared at machine teardown (alongside cancel-flag/registry guards in
  `RegistryGuard::drop` / `install_registries`).
- Tail thunk: TAG_THUNK, state=UNEVALUATED, code_ptr=`lazy_list_chunk`
  (extern "C" fn(vmctx, thunk_ptr) -> head_ptr), captures at
  THUNK_CAPTURED_OFFSET(24): [registry_id: u64, offset: u64]. Raw-int
  captures are safe: GC's evacuation uses `is_in_range(from-space)` so
  non-pointer words pass untouched (same as vmctx tail fields).
- `lazy_list_chunk`: read (id, offset) from captures; materialize K=256
  elements via `value_to_heap`; on `NurseryExhausted` call
  `gc_trigger(vmctx)` and retry once (register all in-flight chunk heap
  ptrs incl. the partial cons spine with RUST_ROOTS mark/truncate before
  triggering — host frames are invisible to the walker); link cons cells
  (cons_tag, 2 fields); terminate chunk with the NEXT tail thunk (offset+K)
  or nil Con when exhausted (then remove registry entry). Return chunk head.
  `heap_force` writes the indirection into the forced thunk afterward
  (standard path).
- Dispatch site: at the `EffectResponseTooLarge` check, if the response
  Value is a cons-spine (walk Con(cons,[h,t]) chain; machine resolves
  cons/nil DataConIds from the DataConTable already available in run()),
  flatten the spine into Vec<Value> (elements only, O(n) Rust clones),
  register, and respond with JUST a tail thunk at offset 0. The resume path
  force_ptr's the response → chunk 1 materializes immediately, rest lazy.
  Non-spine shapes keep the (raised) cap as backstop.

Consequences: `take 5` of a huge glob materializes one chunk ever; `length`
walks chunks while consumed cells become garbage (heap growth + GC handle
it); the cap stops killing legitimate programs.

## Status (2026-06-10 late: BLOCKER SOLVED — feature working, still gated)

- [x] Design validated against layout/GC/heap_force code
- [x] Registry + lazy_list_chunk in host_fns.rs (compiles; GC-rooted)
- [x] Spine probe (iterative) + flatten + dispatch in jit_machine.rs
- [x] Registry clear in RegistryGuard teardown
- [x] Cap raised 10k → 100k for eager shapes
- [x] Tests live (lazy_effect_results.rs all un-ignored, lazy_bisect.rs,
      lazy_minimal_repro.rs, lazy_eager_fallback.rs)
- [x] **BLOCKER SOLVED**. The "pure-JIT spin" was neither JIT nor a spin:
      `resp_val` — the 12k-element response spine — hit `Value`'s RECURSIVE
      destructor at the end of the jit_machine effect arm (~3 stack frames
      per cons cell ≈ 36k frames) → stack-overflow SIGSEGV *outside*
      with_signal_protection → signal handler's no-jmpbuf path exits the
      THREAD silently → the caller waits forever. Every observation is
      explained: spin counters silent (no execution at all), trace silence
      after the clean Val return (the drop runs after), heap validation
      passing (heap was fine), and the minimal repro "passing" (it ran on a
      hand-spawned 8 MiB thread that absorbed the deep drop).
      * Found via: minimal-stack bisect (passed) → MCP-shape variants A/B/C
        (variant A — response never even consumed — still hung, exonerating
        consumption) → watchdog-abort + `gdb -batch -ex run -ex 'thread
        apply all bt'` (gdb may LAUNCH a child under yama ptrace_scope=1;
        only attach is blocked) → 34,620 frames of drop_in_place<Value>.
      * The evidence had been in .tidepool/crash.log the whole time:
        `sig=SIGSEGV addr=...fff8 jmpbuf=null ctx=resuming after effect`.
        Guard-page address, no active protection. ALWAYS check crash.log.
      * Fixes: (1) probe_list_spine (by-ref validate) + dismantle_list_spine
        (by-value iterative move-out — also kills the element clones) in
        jit_machine; long spines never reach a recursive Drop or recursive
        value_to_heap in ANY configuration. (2) Eager gate-off path
        materializes flattened lists iteratively via shared
        host_fns::build_cons_cells (extracted from lazy_list_chunk; GC-safe,
        gc-retry). (3) Signal handler now writes an async-signal-safe stderr
        breadcrumb before the silent thread exit. (4) apply_cont_heap
        k2_stack/result/union_val GC-rooting holes closed (latent, found en
        route: pending continuations were unregistered across forces).
- [ ] Decide default-on: with the blocker root-caused, the gate could flip
      (TIDEPOOL_LAZY_RESULTS=0 to opt out) after a dogfood gauntlet.
- [ ] Stage 2 (zero-copy direction, user-endorsed): handlers park their
      Vec WITHOUT building a Value spine (`respond_lazy`) — kills ToCore
      conversion + flatten clones entirely.
- [ ] Stage 3: element-level COW views — tail = (registry id, offset)
      view cell; head converts ONE element on demand, memoized by thunk
      indirection. A Haskell [Text] over a Rust Vec<String> becomes an
      honest copy-on-read slice.
- [ ] Adopt the `recursion` crate for host-side traversals over deep
      heap/Value trees (node_count was hand-rolled; heap_to_value_inner
      still recurses with a MAX_DEPTH band-aid).

Key layout facts: THUNK_STATE_OFFSET=8, THUNK_CODE_PTR_OFFSET=16,
THUNK_INDIRECTION_OFFSET=16, THUNK_CAPTURED_OFFSET=24; thunk size
24+8*ncaptures; heap tags Closure=0 Thunk=1 Con=2 Lit=3. Con layout:
CON_TAG_OFFSET(u64), CON_NUM_FIELDS_OFFSET(u16), CON_FIELDS_OFFSET+8*i.
value_to_heap is in heap_bridge.rs:239 (bump-only, no GC).
