# tidepool-runtime

High-level Rust API for compiling and running Haskell source through
`tidepool-extract`: compile-to-Core, JIT-execute, cache the compiled
artifacts on disk, and (via `session`) drive a resident multi-turn session on
top of all three.

## What's here

- **Compile + run** (`lib.rs`) — `compile_haskell`/`compile_haskell_salted`
  (source → `CoreExpr` + `DataConTable`); `compile_and_run`/
  `compile_and_run_with_nursery_size` (compile, then JIT-execute against a
  `DispatchEffect` handler stack); `compile_and_run_pure`/
  `compile_and_run_pure_salted` (no effect dispatch, for `Eff`-free
  programs); `compile_and_run_cancellable` (hands back a `CancelHandle`
  before the blocking run starts, for a watchdog to abort); and the
  suspend/resume pair `compile_and_run_suspendable`/`resume_suspended_turn`
  (thread-less suspension at an `Ask` boundary — the machine is handed back
  as data instead of parking a thread).
- `artifacts` — the one policy-bearing `tidepool-extract` invocation front
  door (`CompileInvocation`/`compile_invocation`); `compile_targets`
  compiles several targets in one GHC session, for the harness turn lane.
- `cache` — filesystem cache for compiled artifacts (`invocation_key`,
  `artifacts_load`/`artifacts_store`).
- `session` — the resident-session machinery built on top of the above:
  `SessionEngine`/`EngineConfig` (the turn driver — pool sizing, admission,
  continuation ids), `SessionLib` (declaration accumulation across turns),
  and `PersistentSession`/`ResidentSession` (the checked-out session
  lifecycle).
- `toolchain` — the one locator for the extract binary and the Haskell
  stdlib tree, plus the deploy handshake that refuses to serve a mismatched
  pair.
- `paths` — canonical on-disk locations (cache dir, config dir,
  project-local `.tidepool/`).
- `diag` — parses `tidepool-extract`'s diagnostics report and renders it to
  human-facing text.
- `failclass` — classifies a compile/run/session error into `FailureClass`
  (`UserHaskell`/`Runtime`/`Infra`/`VersionSkew`) × `Phase`.
- `render` — `EvalResult`/`value_to_json`: turns an evaluated `Value` into
  JSON.
- `timing` — per-turn latency attribution: the stage vocabulary and the one
  emitter every instrumented call site uses.
