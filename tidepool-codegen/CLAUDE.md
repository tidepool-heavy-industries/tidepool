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
