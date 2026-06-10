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

## Status (2026-06-10, parked WIP — gated behind TIDEPOOL_LAZY_RESULTS=1)

- [x] Design validated against layout/GC/heap_force code
- [x] Registry + lazy_list_chunk in host_fns.rs (compiles; GC-rooted)
- [x] Spine probe (iterative) + flatten + dispatch in jit_machine.rs
- [x] Registry clear in RegistryGuard teardown
- [x] Cap raised 10k → 100k for eager shapes (ungated; strict improvement)
- [x] Tests written (tidepool-runtime/tests/lazy_effect_results.rs);
      small_responses_stay_eager live; lazy tests #[ignore]d as WIP
- [ ] **BLOCKER**: pure-JIT spin after PARTIAL consumption of a lazy list
      under the MCP wrapper. Debugging ledger:
      * Full traversal WORKS: filtered_fold streamed 118/118 chunks
        (offsets 0..29952 at n=30k) — materializer, GC interplay, chunk
        boundaries, and exhaustion/nil are all sound.
      * `take 3` / `length (take 3 xs)` hang: chunk 1 materializes once,
        then a loop that makes NO host calls (trampoline + heap_force spin
        counters silent; TIDEPOOL_TRACE=calls shows 3 closure calls
        consuming cons cells, a clean Val(…) return, then silence).
      * TIDEPOOL_TRACE=heap validation passes on all touched objects.
      * Con layout verified identical across tidepool-heap and
        tidepool-codegen layout modules (8/16/24).
      * Incidental fixes landed separately (commit 8deee87): iterative
        node_count; GC-rooted apply_cont ENTRY forces (forcing a response
        thunk from the all-host resume chain collected the continuation).
      * Hypothesis space remaining: interaction between the lazy tail
        thunk and the effect machine's parse_result / Val unwrapping when
        the tail is NEVER forced (take leaves the chunk-1 tail thunk live
        inside the retained list; something in the wrapper may walk it via
        a path that mishandles host-code thunks — e.g. an emitted
        force-loop that expects thunk code to be JIT code with vmctx
        conventions?). Next session: reproduce OUTSIDE the MCP wrapper
        with a minimal effect stack; ptrace needs enabling
        (kernel.yama.ptrace_scope) for a direct backtrace; consider rr.
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
