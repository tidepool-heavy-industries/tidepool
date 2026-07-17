# tidepool-codegen — Cranelift JIT compiler + effect machine

Compiles `CoreExpr` to Cranelift-backed state machines and drives the effect
machine at the JIT↔Rust boundary. See the repo-root `CLAUDE.md` for the project
map and locked decisions.

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
| `RUST_LOG=tidepool::effects=debug` (legacy `TIDEPOOL_TRACE_EFFECTS=1`) | Effect machine | Effect dispatch at the JIT↔Rust boundary | Effect results arriving wrong / lazy-result suspicion |
| `TIDEPOOL_LAZY_RESULTS=0` | Effect machine | Kill-switch: disables lazy effect results (typed Stream/List channel) | Bisecting whether a bug is in the lazy-results path |
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
  formerly `runtime_case_trap`. A value's constructor tag or heap shape didn't
  match what was compiled. Three `ShapeTrapKind` callers: a case scrutinee
  matching no alternative (`CaseMiss`, `emit_case_trap` in `src/emit/case.rs`), and
  the numeric-unbox guards for wrong Con arity (`BoxingArity`) / wrong literal
  class (`LitClass`) in `src/emit/primop.rs`. The `kind` selects the breadcrumb
  label (`[CASE TRAP]` / `[SHAPE TRAP: …]`); all three surface
  `RuntimeError::CaseTrap` and print the enclosing fn + scrutinee tag + expected
  alt tags. `emit_case_trap` replaced a bare `trap user2` (→ `ud2` → SIGILL) — it
  CALLs the host fn, uses its poison return, and continues; if a poison/error
  already cascaded in it returns poison immediately, and a lazy poison-closure
  scrutinee is triggered to set the error flag.

- **Runtime domain errors** (division by zero, `Prelude.chr: bad argument`) →
  the `runtime_error`/`runtime_error_with_msg` machinery, same as a Haskell
  `error` call. The div/`chr` guards in `src/emit/primop.rs` used to emit bare
  `trap`/`trapnz` (→ SIGILL); they now raise a clean `RuntimeError` and substitute
  a safe operand so execution continues to a placeholder value the pending error
  preempts.

Almost all PrimOpKind variants are implemented; a clean runtime error is
surfaced when `with_signal_protection` returns, instead of crashing. (A genuine
SIGILL/SIGSEGV now points at heap corruption or a bad pointer — no routine
language-level error reaches a signal.) Two variants are NOT emitted:
`TagToEnum | SeqOp => Err(NotYetImplemented(..))` (`emit/primop.rs:2124`).
`TagToEnum` is desugared upstream (`haskell/src/Tidepool/Translate.hs`, grep the
`pop == TagToEnumOp` guard; ~L1463),
so that half is an unreachable backstop. `SeqOp` is a real differential gap —
handled by the eval oracle (`tidepool-eval/src/eval.rs:1539`) but NOT the JIT.
The proptest generator (`tidepool-testing`) does not currently emit `SeqOp`
(checked 2026-07-07), so this gap isn't exercised today; if the generator is
extended to cover it, either implement `SeqOp` in the JIT or exclude it from
generation explicitly.

The boxed-array primops (`IndexArray`/`ReadArray`/`IndexSmallArray`/etc.) are
the same status class as `SeqOp`: JIT-real (`emit/primop.rs` implements them)
but eval-unsupported (`tidepool-eval/src/eval.rs`'s tree-walker has no boxed-
array `Value` variant — only the unboxed `ByteArray`). Also unexercised by the
proptest generator today, so likewise latent rather than firing.
