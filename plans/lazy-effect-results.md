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

## Status (2026-06-10 late: BLOCKER SOLVED — DEFAULT-ON, opt out via TIDEPOOL_LAZY_RESULTS=0)

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
- [x] Default-on (author-approved after live MCP gauntlet: 6.3k-entry glob
      responses, sieve, error surfacing, rsFn all green on the eager path).
      TIDEPOOL_LAZY_RESULTS=0 opts out to eager-iterative materialization.
- [x] Stage 2 SHIPPED (2026-06-10): `Response::{Complete, Stream}` typed
      channel (breaking trait change, absorbed by `cx.respond`);
      `cx.respond_stream(impl IntoIterator)` parks the ITERATOR — the
      iterator is the cursor, thunk memoization the linearizer; no Value
      spine, no flatten, no offset bookkeeping. Elements convert per-pull
      at chunk time (take 3 of 12k ⇒ ≤256 conversions, pull-counter
      verified). Registry unified: ParkedStream + ReadySource (dismantled
      Complete spines ride the same chunk materializer); tail thunks carry
      id only. Producer panics catch_unwind'd at the JIT boundary; cancel
      safepoint per pull; infinite iterators are legitimate infinite
      Haskell lists (kill-switch mode drains them to a clean TooLarge).
      MCP handlers flipped: glob, grep, sgFind, sgRuleFind, listDirectory.
      Result-side twin bugs closed in the same wave: Value gets an
      ITERATIVE Drop (E0509 fallout ~15 mechanical sites), heap_to_value
      walks spines iteratively with per-frame GC rooting (stale-parent
      hole fixed), Array payloads re-derived per element.
- [x] Stage 3a SHIPPED (2026-06-10): element-level copy-on-read.
      ValueSource gains an optional random-access facet (len()/get(idx));
      indexed sources (cx.respond_list(Vec<T>), dismantled Complete
      spines via ReadySource, custom from_source impls) build chunks
      whose cons-cell HEADS are (id, idx) host thunks — forcing one head
      converts ONE element, memoized by thunk indirection. Measured:
      take 3 of 12k ⇒ exactly 3 conversions; length of 12k ⇒ ZERO
      conversions (also proves JIT case-dispatch forces cells, not
      fields); filter ⇒ all (correct contrast). Sequential iterators
      keep stage-2 chunked conversion. Indexed registry entries live to
      machine teardown (outstanding element thunks reference them).
      Element conversion panic-guarded at the JIT boundary.
      MCP handlers use respond_list (glob/grep/sg/listDirectory).
- [x] `recursion` crate adopted where it FITS (2026-06-10):
      value_to_heap is now a fallible hylomorphism
      (try_expand_and_collapse over ValueFrame) — arbitrarily deep bushy
      Values convert on a 64 KiB thread (regression test). The forcing
      heap_to_value walker stays custom: per-frame GC rooting is
      inherently effectful and does not belong in a generic fold (the
      spine loop + RootScope discipline covers it). Remaining recursion:
      heap_to_value bushy paths (depth-capped at 10k, acceptable),
      render/json walks (depth-capped, list-iterative).
- [ ] Stage 3b (only if profiling demands): true zero-copy — heap Text
      payloads pointing into parked Rust data; needs foreign-pointer GC
      support (pinning/keepalive design doc first).

Key layout facts: THUNK_STATE_OFFSET=8, THUNK_CODE_PTR_OFFSET=16,
THUNK_INDIRECTION_OFFSET=16, THUNK_CAPTURED_OFFSET=24; thunk size
24+8*ncaptures; heap tags Closure=0 Thunk=1 Con=2 Lit=3. Con layout:
CON_TAG_OFFSET(u64), CON_NUM_FIELDS_OFFSET(u16), CON_FIELDS_OFFSET+8*i.
value_to_heap is in heap_bridge.rs:239 (bump-only, no GC).
