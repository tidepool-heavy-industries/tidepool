# Spec: engine residency — persistent machines + fragment suspend + session map

Goal: a session = a resident `JitEffectMachine` served by the E2 stow
engine: turns run fragments against the retained heap; an ask STOWS the
machine as data (no parked thread); completion returns the machine to its
session slot. This is flipping designed-in seams, not new machinery.

## ANTI-PATTERNS

- DO NOT touch `tidepool-repl` — it keeps its parked-thread mechanism
  unchanged. This work lives in `tidepool-runtime` (+ the new harness
  crate's registry).
- DO NOT weaken the stowed-XOR-running discipline that justifies
  `unsafe impl Send for JitEffectMachine` (jit_machine.rs:113–122). Every
  new code path must go through the registry; no side-channel machine
  access (e.g. a bindings peek) while running.
- DO NOT remove or relax the L7 `suspended_continuation.is_none()` asserts
  (jit_machine.rs:511, 603, 776, 954, 1087, 1232, 1387) — that is segment
  40's job, with GC rooting. In THIS segment a suspended session simply
  does not accept new runs.
- DO NOT hold the registry lock across an `.await` (parking_lot is not
  async-aware — the repl's state.rs discipline: lock → inspect → move
  owned values out → unlock → await).
- DO NOT build decl-plane cloning — cut from R0.

## READ FIRST

- `tidepool-runtime/src/session/engine.rs` — FULL module docstring (E2
  threadless suspension, E3 timeout-yield, and the "end-state registry"
  paragraph at ~46–61 that names this exact unification as the intended
  next step). Then: `Retention` (117–122, `Persistent` variant unused),
  `StowedResume` (145), `make_resume_closure` (1023–1120, note the
  completion arm at 1082–1087 where the machine is DROPPED — the seam),
  `resume` validate-before-consume (621–754, esp. 684–698).
- `tidepool-codegen/src/jit_machine.rs`: `run_suspendable` (590–636),
  `resume_suspended` (651–699, re-installs GC state on ANY thread),
  `run_fragment`/`add_function` (~864–929), `install_registries` (417–431),
  `RegistryGuard`/`reclaim_session_heap` (240–280).
- `tidepool-repl/src/state.rs` module docstring — the lifecycle enum +
  mutex discipline to port (per-session already; do not copy repl code
  wholesale, port the pattern).
- `tidepool-repr` `SessionId` (exists; the repl uses it only to namespace a
  directory — it becomes the registry key here).

## MECHANISM / STEPS

1. **Persistent retention**: implement the `Retention::Persistent` path —
   on turn completion the `(machine, table, handlers)` go back to the
   session slot instead of dropping inside the `FnOnce`. Prefer replacing
   the closure-capture shape for resident sessions with a per-session
   owned struct + direct `run_*` calls (the engine docstring's "end-state
   registry": `{machine, ModuleEnv, render policy, retention, pool slot}`);
   keep the oneshot `FnOnce` path untouched for the eval server.
2. **Fragment × suspend composition**: drive resident turns through the
   suspend-capable entry so an Ask mid-fragment yields
   `Suspended{machine, request}` instead of dispatching. `run_suspendable`
   and `run_fragment` share `drive_effect_loop`; the composition has never
   been exercised — this step is mostly tests proving it (suspend inside a
   bind fragment, resume completes the bind, binding lands).
3. **Session registry** (harness crate): `HashMap<SessionId, Slot>` where
   `Slot = Idle(ResidentSession) | Running | Suspended{machine, hole}`.
   Lifecycle transitions atomic at the dispatch boundary; suspended
   sessions reject new runs with a clear error (until segment 40 lands
   nested child runs).
4. **Validate-before-consume carries over**: reuse the engine's atomic
   validate-then-remove under one lock (engine.rs:684–698) — for typed
   holes the "validation" is the child's successful compile+NF-force, but
   the not-consumed-on-failure semantics and the three-way resume errors
   (repl server.rs precedent) port as-is.

## VERIFY

- New tests in tidepool-runtime (GHC-heavy tier — needs
  `--ignore-default-filter`, `TIDEPOOL_EXTRACT` set): multi-turn resident
  session accumulates bindings across suspend/resume; machine reuse after
  a resumed turn completes; suspended session rejects new `run` cleanly;
  resume on wrong/stale continuation id errors without consuming.
- `cargo nextest run -p tidepool-runtime --ignore-default-filter` +
  `scripts/battery.sh` green.

## DONE

A resident session survives: run → suspend at `returnControl` → (thread
exits, nothing parked) → resume with a value → turn completes → machine
back in slot → next turn sees prior bindings. Eval server behavior
unchanged (its suite green).
