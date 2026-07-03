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

For the Haskell-extract knobs (a separate process: `DUMP_CLOSED`, `VARID_AUDIT`,
`JOINREC_DEBUG`, `IFACE_DEBUG`) see `haskell/CLAUDE.md`.

| Knob | Layer | What it shows | Reach for it when |
|------|-------|---------------|-------------------|
| `RUST_LOG=tidepool::calls=trace` (legacy `TIDEPOOL_TRACE=calls`) | JIT runtime | Every closure call: name, arg, result (`src/debug.rs`) | Tracing which function received/returned a bad value (e.g. wrong type at a case dispatch) |
| `RUST_LOG=tidepool::heap=trace` (legacy `TIDEPOOL_TRACE=heap`) | JIT runtime | `calls`+`scope` + heap-object validation before use | Suspected heap corruption / bad pointer breadcrumbs |
| `RUST_LOG=tidepool::effects=debug` (legacy `TIDEPOOL_TRACE_EFFECTS=1`) | Effect machine | Effect dispatch at the JIT↔Rust boundary | Effect results arriving wrong / lazy-result suspicion |
| `TIDEPOOL_LAZY_RESULTS=0` | Effect machine | Kill-switch: disables lazy effect results (typed Stream/List channel) | Bisecting whether a bug is in the lazy-results path |
| `RUST_LOG=tidepool::fp=debug` (legacy `TIDEPOOL_FP_DEBUG=1`) | Runtime cache | Binary-fingerprint memo keys + sidecar hit/miss (`tidepool-runtime/src/cache.rs`) | Stale-cache suspicion. Note: kernel ctime has ~3ms granularity — sub-tick writes legitimately memo-hit |
| `NONCE=<x>` / `FORCE=1` | `repro313` test | Cache-busting fresh compile / forces Int result inside the user continuation | Re-running the #313 regression gate against a fresh compile |

Always-on breadcrumbs (`[CASE TRAP]`, `[BUG]` bad-pointer lines on stderr) stay
unconditional: they fire only on actual compiler bugs, which must be loud. If you
see one, that's a reportable codegen bug, not user error.

**Case trap = a value matched no branch, not a missing primop.** All `PrimOpKind`
variants are implemented (the `_ =>` catch-all is unreachable). An exhausted/empty
case no longer emits a bare Cranelift `trap user2` (→ `ud2` → SIGILL): `emit_case_trap`
(`src/emit/case.rs`) now emits a CALL to the `runtime_case_trap` host fn
(`src/host_fns.rs`), uses its return value, and continues. That host fn prints the
always-on `[CASE TRAP] in compiled fn: <name>` breadcrumb, then returns
`error_poison_ptr()` — surfacing a clean runtime error (detected when
`with_signal_protection` returns) instead of crashing. If a poison/error already
cascaded into the case it returns poison immediately; a lazy poison-closure
scrutinee is triggered to set the error flag. Root cause still varies (constructor
tag mismatch, unexpected value shape) — the breadcrumb names the enclosing fn and
dumps the scrutinee tag + expected alt tags. (A genuine SIGILL/SIGSEGV now points
at heap corruption or a bad pointer, not the case trap.)

---

## JIT Emit Gotchas (emit/expr.rs)

Reference for anyone touching `src/emit/expr.rs` or adjacent emit modules. Each item documents a hard-won fix; re-opening any of these reproduces the original failure.

**LetRec 5-phase ordering**: The phases must run in order. Phase 3a: compile Lam bodies, fill code pointers + partial captures. Phase 3b: fill Con fields not referencing simple bindings. Phase 3c: evaluate deferred simple bindings with incremental capture + Con filling. Phase 3a': fill remaining closure captures. Phase 3d: fill any remaining deferred Con fields. Critical invariant in Phase 3c: after evaluating each simple binding, (1) fill pending closure captures, then (2) check deferred Cons whose simple-binding deps are all now satisfied and fill ALL their fields immediately. Deferring step (2) causes SIGSEGV when a later simple binding calls a closure that case-matches a Con whose fields haven't been filled yet.

**Strict let / error bindings**: GHC Core hoists `error "..."` calls into shared `let` bindings used by impossible branches (e.g. `let err = error "Failure in balanceL" in case … of { … -> err; … -> normal }`). These are lazy thunks in Haskell; the JIT evaluates them eagerly and hits the error. Fix in `emit_letrec_phases` and the LetNonRec path: `rhs_has_error_sentinel()` checks if any free vars of the RHS are error sentinel VarIds (tag `0x45` in high byte). If so, bind to a poison closure instead of emitting the body. Applied in LetNonRec, LetRec all-simple, and LetRec deferred-simple paths.

**Do NOT stripBoxCon wrapper args**: GHC DataCon wrappers take boxed args (e.g. `I# n`) while workers store unboxed. A prior `stripBoxCon` helper that stripped `I#` from wrapper args before storing in NCon was reverted — it caused Text `Array` fields to hold bare `Int#` instead of `I# n`, producing CASE TRAP when downstream code case-matched expecting `I#`. Current state: no stripping anywhere. The recursive `unbox_*` helpers (PR #120) handle both boxed and unboxed transparently when primops need a raw `Int#`. If a proposal to strip wrapper boxes for efficiency appears, reject it.

**LetRec thunk sibling capture drop**: `emit_thunk` creates a fresh `EmitContext`, dropping free vars not present in the outer env at that moment. When a deferred simple binding is thunkified (because it's a dependency of a deferred Con), sibling deferred simple bindings not yet materialized in env are silently dropped from captures → `unresolved_var_trap` at runtime. Manifested as `T.split` returning `<closure>` instead of `""` (`Data.Text.Array.empty` was the dropped sibling). Fix in `emit_letrec_phases`: before thunkifying, check whether any free vars of the RHS are sibling deferred simple bindings not yet in env. If so, fall through to the work-stack path (evaluates in correct LIFO dependency order) instead of thunkifying.
