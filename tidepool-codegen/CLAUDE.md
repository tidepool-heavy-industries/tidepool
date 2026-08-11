# tidepool-codegen — Cranelift JIT compiler + effect machine

Compiles `CoreExpr` to Cranelift-backed state machines and drives the effect
machine at the JIT↔Rust boundary. See the repo-root `CLAUDE.md` for the project
map and locked decisions.

## Nested child runs on a suspended machine

A parent turn suspended at a typed yield (`runLLMTurn`/`Ask`) can host
SEQUENTIAL child fragment runs on the SAME machine — reading the parent's
bindings zero-copy — while its stowed continuation is a REGISTERED GC ROOT. The
full invariant is in the `jit_machine.rs` module docstring; the essentials:

- The GC root assembly (`perform_gc`, `host_fns/gc.rs`) folds SIX sources:
  frame-walked stack, run-scoped `rust_roots`, session `persistent_roots`, the
  nested-child-scoped `stowed_roots` (`MachineState`), write-barrier
  `remembered_slots` (tenured-array payload slots touched by a later
  `writeSmallArray#`/`WriteArray`/`casSmallArray#`/copy), and the vmctx
  tail-call slots. `stowed_roots` is DELIBERATELY separate from
  `persistent_roots` so intent is auditable: a persistent root is a
  machine-lifetime tenured value; a stowed root is a *transient* parent
  continuation rooted only while a child runs.
- `JitEffectMachine::run_child_fragment{,_pure}` are the ONLY sanctioned run
  entries while suspended. They go through `enter_nested_child`, which moves the
  continuation into a heap-stable `Box` cell, registers it in `stowed_roots`,
  and (by emptying `suspended_continuation` for the child's duration) lets the
  child drive through the plain entries whose L7 `is_none()` asserts then pass.
  The `NestedChildGuard` drops AFTER the child's `RegistryGuard` reclaim, so it
  reads the GC-current continuation pointer back out against the POST-child heap.
- The L7 asserts on the plain entries are UNCHANGED and still fire for the
  illegal state (a plain run while a continuation is stowed unregistered).
- `resume_suspended` NF-forces (A5) a data-kinded answer BEFORE consuming the
  continuation: a bottom (residual unforced thunk) is rejected as a retryable
  error WITHOUT consuming, so the caller can resume again with a fixed answer.

A suspendable turn completes under one of the SAME four result-materialization
policies the plain routes use — `Value`, `Bind{forced}`, `Project{n_fields}`
(multi-bind), `Render{field0_forced}` (bind + render in one run) — each with a
run entry (`run_fragment_suspendable{,_binding,_projected,_render}`) and a
resume sibling (`resume_suspended{,_binding,_projected,_render}`). There is one
implementation (`JitEffectMachine::materialize`), reached by both families, so
a turn behaves identically whether it completed in its first run or after any
number of ask suspensions — including `Render`'s load-bearing bridge-field1-
BEFORE-tenure-field0 ordering, which is what keeps an aliased `(it, toWire it)`
intact. The PARKED (registry) path covers only the first two: `ParkKind` has no
`Project`/`Render` spelling. Per-policy table in the `jit_machine.rs` module
docstring; suspend-then-complete coverage in
`tests/suspendable_materialization.rs`.

A machine can ALSO hold multiple independently parked continuations in the
continuation registry (realm machinery): each `ContinuationFrame` is a
registered stowed root, `stowed_roots_count() == parked_count()` holds at
quiescence (debug_asserted at every registry mutation), handled-prefix
compatibility is exact equality enforced at entry, and the single-slot path
above and the parked registry NEVER mix on one machine. Downstream consumers
read `plans/post-restart/realm-lanes/continuation-parking-contract.md` —
everything else in the registry is internal and free to churn.

The adversarial suite (`tests/nested_child_gc_rooting.rs`, run with
`TIDEPOOL_GC_POISON`/`TIDEPOOL_HEAP_VERIFY` on) is the memory-safety gate: child
GC + heap doubling with a live suspended parent, deep-verified resume, decl
accretion inert for the parent, bottom-not-consuming, L7 misuse panic, and
value-plane tenure across suspend → child GC → resume.

## Diagnostics — JIT runtime / effect machine / cache

Env-gated, OFF by default. The Rust JIT-runtime traces use `log` + `env_logger`
(per-subsystem `tidepool::*` targets) driven by `RUST_LOG`. The legacy
`TIDEPOOL_TRACE*`/`TIDEPOOL_FP_DEBUG` vars are still honored as back-compat
aliases (mapped in `tidepool_codegen::debug::init_logging`). Example:
`RUST_LOG=tidepool::calls=trace,tidepool::heap=trace`.

For the Haskell-extract knobs (a separate process: `TIDEPOOL_DUMP_CLOSED`,
`TIDEPOOL_VARID_AUDIT`, `TIDEPOOL_JOINREC_DEBUG`, `TIDEPOOL_IFACE_DEBUG`) see
`haskell/CLAUDE.md`.

| Knob | Layer | What it shows | Reach for it when |
|------|-------|---------------|-------------------|
| `RUST_LOG=tidepool::calls=trace` (legacy `TIDEPOOL_TRACE=calls`) | JIT runtime | Every closure call: name, arg, result (`src/debug.rs`) | Tracing which function received/returned a bad value (e.g. wrong type at a case dispatch) |
| `RUST_LOG=tidepool::heap=trace` (legacy `TIDEPOOL_TRACE=heap`) | JIT runtime | `calls`+`scope` + heap-object validation before use | Suspected heap corruption / bad pointer breadcrumbs |
| `RUST_LOG=tidepool::effects=debug` (legacy `TIDEPOOL_TRACE_EFFECTS=1`) | Effect machine | Effect dispatch at the JIT↔Rust boundary | Effect results arriving wrong |
| `TIDEPOOL_HEAP_VERIFY=1` (tests: `set_heap_verify`) | GC | Post-GC walk of the packed to-space; panics on the first invariant violation (from-space pointer, size-wrap, bad tag) | Corruption INSIDE evacuated objects. Blind to missed stack roots — pair with GC_POISON |
| `TIDEPOOL_GC_POISON=1` (tests: `set_gc_poison`) | GC | Fills from-space (and the doubling path's intermediate space) with 0xDD before freeing | Timing-dependent SIGSEGVs: a stale pointer the GC missed then reads tag 221 DETERMINISTICALLY (e.g. "application of non-closure (tag=221)") instead of sometimes working |
| `RUST_LOG=tidepool::fp=debug` (legacy `TIDEPOOL_FP_DEBUG=1`) | Runtime cache | Binary-fingerprint memo keys + sidecar hit/miss (`tidepool-runtime/src/cache.rs`) | Stale-cache suspicion. Note: kernel ctime has ~3ms granularity — sub-tick writes legitimately memo-hit |
| `NONCE=<x>` / `FORCE=1` | `repro313` test | Cache-busting fresh compile / forces Int result inside the user continuation | Re-running the #313 regression gate against a fresh compile |

Always-on breadcrumbs (`[CASE TRAP]`/`[SHAPE TRAP: …]`, `[BUG]` bad-pointer lines
on stderr) stay unconditional: they fire only on actual compiler bugs, which must
be loud. If you see one, that's a reportable codegen bug, not user error.

**The JIT emits no bare `trap`s — every fault routes through a host call.** Two
families:

- **Shape/tag-mismatch traps** → `runtime_shape_trap` (`src/host_fns/errors.rs`),
  A value's constructor tag or heap shape didn't
  match what was compiled. Five `ShapeTrapKind` callers: a case scrutinee
  matching no alternative (`CaseMiss`, `emit_case_trap` in `src/emit/case.rs`);
  the numeric-unbox guards for wrong Con arity (`BoxingArity`) / wrong literal
  class (`LitClass`); and the address/byte-array unbox guards (`AddrKind`,
  `ArrayKind`) that reject a non-address raw kind or an untagged payload
  before any Addr#/array-consuming primop dereferences it (shared
  `unwrap_boxing_chain`; pinned by `tests/ffi_strlen_unbox_hardening.rs` and
  `tests/ffi_bytearray_unbox_hardening.rs`) — all in `src/emit/primop.rs`.
  The `kind` selects the breadcrumb label (`[CASE TRAP]` / `[SHAPE TRAP: …]`);
  all five surface `RuntimeError::CaseTrap` and print the enclosing fn +
  scrutinee tag + expected alt tags. `emit_case_trap` emits no bare `trap` (so no SIGILL) — it
  CALLs the host fn, uses its poison return, and continues; if a poison/error
  already cascaded in it returns poison immediately, and a lazy poison-closure
  scrutinee is triggered to set the error flag.

  **`unbox_addr` does not guard the address itself, so raw dereferences need
  their own check.** Its `SsaVal::Raw(v, tag)` branch trusts a *static*
  literal tag (a compile-time label, not a runtime check on `v`) — correct
  there, since a legitimate `Addr#` computation (`plusAddr#`, `eqAddr#`,
  `minusAddr#`) must be free to hold a null or out-of-range address without
  tripping a trap. The hazard is the class of `Addr#`-consuming primop that
  never calls a host fn (`IndexCharOffAddr`, `IndexWord8OffAddr`,
  `WriteWord8OffAddr`, `IndexAddrOffAddr`, `IndexInt8OffAddr`,
  `IndexWord32OffAddr`, `IndexWideCharOffAddr`, `WriteWideCharOffAddr`): those
  dereference the raw pointer through a Cranelift `load`/`store` with
  `MemFlags::trusted()`, so a bad address is an uncaught SIGSEGV rather than
  the clean `RuntimeError` every other fault here surfaces.
  `emit_addr_deref_guard` (`src/emit/primop.rs`) closes it with a runtime
  null/low-address check immediately before each of those load/stores,
  reusing `ShapeTrapKind::AddrKind`. **A new `Addr#`-dereferencing primop must
  call it.** Pinned by `tests/addr_deref_unbox_hardening.rs` (A/B'd against a
  real SIGSEGV: `IndexAddrArray` reading a zero-filled `ByteArray#` slot as an
  address).

- **Runtime domain errors** (division by zero, `Prelude.chr: bad argument`) →
  the `runtime_error`/`runtime_error_with_msg` machinery, same as a Haskell
  `error` call. The div/`chr` guards in `src/emit/primop.rs` raise a clean
  `RuntimeError` (no bare `trap`/`trapnz`, so no SIGILL) and substitute
  a safe operand so execution continues to a placeholder value the pending error
  preempts.

  An **unresolved-external poison** (the `0x45` kind-4 sentinel the extract
  bakes when it cannot resolve a symbol) is in this family and NAMES the
  symbol: the emitted node carries a 48-bit identity slot
  (`VarId::sentinel()`), `meta.cbor`'s `poisoned` table maps slot -> qualified
  name, `register_poisoned_externals` loads it at metadata read time, and
  `emit/expr.rs` resolves the slot when it emits the lazy poison — so forcing
  one raises `RuntimeError::UnresolvedExternal("Dep.helper")` instead of an
  anonymous kind-4 `TypeMetadata`. An UNNAMED kind-4 now means one of: a
  pre-2.1 payload, a genuine `$tc*`/`$trModule*` type-metadata sentinel, or a
  slot the multi-target metadata merge dropped as ambiguous.

Almost all PrimOpKind variants are implemented; a clean runtime error is
surfaced when `with_signal_protection` returns, instead of crashing. (A genuine
SIGILL/SIGSEGV now points at heap corruption or a bad pointer — no routine
language-level error reaches a signal.) Two variants are NOT emitted:
`TagToEnum | SeqOp => Err(NotYetImplemented(..))` (`emit/primop.rs:2170`).
`TagToEnum` is desugared upstream (`haskell/src/Tidepool/Translate.hs`, grep the
`pop == TagToEnumOp` guard; ~L1796),
so that half is an unreachable backstop. `SeqOp` is a real differential gap —
handled by the eval oracle (`tidepool-eval/src/eval.rs:1546`) but NOT the JIT.
It is latent rather than firing only because the proptest generator
(`tidepool-testing`) emits no `SeqOp`; **extending the generator to cover it
means either implementing `SeqOp` in the JIT or excluding it from generation
explicitly.**

The boxed-array primops (`IndexArray`/`ReadArray`/`IndexSmallArray`/etc.) are
the same status class as `SeqOp`: JIT-real (`emit/primop.rs` implements them)
but eval-unsupported (`tidepool-eval/src/eval.rs`'s tree-walker has no boxed-
array `Value` variant — only the unboxed `ByteArray`). Also unexercised by the
proptest generator today, so likewise latent rather than firing.
