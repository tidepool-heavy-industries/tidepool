# Implementation orchestration — exo fan-out

Not a linear milestone sequence: a scaffold → fork-wave → merge plan for exo
subagents (opus for the load-bearing workstreams, sonnet for the mechanical
reshapes). Root scaffolds frozen contracts + the rename, commits conflict-free,
forks one TL/dev per workstream in its own worktree, integrates with `merge` +
a build gate after each fold.

## Locked decisions (this session)

- **Harness monad = `returnControl` reused, inverted.** `loop :: State -> Harness State`
  runs as a suspendable eval; `runLLMTurn @A` suspends it exactly like
  `returnControl`; the model is the *answerer* of the hole via a nested
  multi-turn Agent session (reusing `drive_turn`/`run_to_hole_or_done`);
  `finalize x` resumes `loop` with `x` in-heap via `run_child`.
- **`finalize` = distinct verb, in-heap, RELAXES the no-function-arrow rule.**
  It crosses via `run_child` (no JSON round-trip), so it may carry closures /
  effectful actions / nonserializable values, unlike `returnControl`.
- **`State` = polymorphic author-defined `(ToJSON s, FromJSON s) => s`,** from day one.
- **Rename `returnControl` → `runLLMTurn` now,** in the scaffold, before the wave.

## Scaffold decisions (recommended — confirm before spawning)

- **Placement:** new modules *inside* `tidepool-harness` (needs `pub(crate)`
  reach into `drive_turn`, `run_child`, `classify_hole`, the providers) + a new
  binary. Not a new crate (would force pub-exporting half the internals).
- **`finalize` wiring:** a **new, distinct interposed effect** (its own GADT
  constructor + union-tag, appended alongside `Ask`, serviced by the harness —
  no `tidepool-handlers` handler). It is semantically distinct (a terminal
  handoff, not an elicitation), so it earns its own effect — but it **shares**
  `Ask`'s suspend-as-data / payload-classify / resume machinery rather than
  copying it. **Code shared, not duplicated.**

## Scaffold phase (root; sequential; one commit, conflict-free)

**S1 — Skeleton.** New module tree in `tidepool-harness/src/selfharness/` +
`tidepool-web/src/bin/tidepool-selfharness.rs` (or reuse the harness bin with a
subcommand). Empty modules + `pub` wiring so `cargo check` passes.

**S2 — Rename `returnControl`→`runLLMTurn`** across all layers (recon C):
`tidepool-mcp/src/effect_defs.rs:620-666` (verb names + doc), the
`isReturnControl*Var` predicates `haskell/src/Tidepool/Translate.hs:2674-2696`,
the `SYSTEM_FRAMING`/`hole_card` prose `tidepool-harness/src/engine.rs:164-250`,
doc/error strings in `harness.rs`/`compile.rs`/`tree.rs`, and the two test files
(`return_control_sidecar.rs`, `acceptance_return_control.rs` + filenames). Wire
shape (`typedSite`/`fork`/`fan`/`prompts`, `AskWith`, `asks.json`) unchanged.
Verify: `scripts/battery.sh` green (rename is behavior-preserving).

**S3 — Freeze contracts as stubs (`unimplemented!()` / typed holes):**
- Effect-stack SHAPE (frozen as a contract; the effect *declarations* + the
  Ask-machinery factoring are **WS-B**, not scaffold): **Harness** =
  `Eff '[RunLLMTurn]` — orchestration-only for v1 (base effects appended later,
  as needed, never prematurely). **Agent** = the existing full base stack +
  `Ask` + `Finalize`. `RunLLMTurn` and `Finalize` are **distinct effects that
  SHARE `Ask`'s suspend/classify machinery** (`runLLMTurn` is split OUT of `Ask`,
  where the rename left it as a verb). `fork` deferred. Scaffold freezes only the
  **Rust seams** (next bullet); WS-B builds the Haskell effects.
- Rust seam for `finalize` (the Haskell `Finalize` effect + Ask factoring are
  WS-B): add a `HoleRouting::Finalize { site, ty }` variant in
  `engine.rs:82-124` + a placeholder `classify_hole` arm (`unimplemented!()`), so
  the driver + classifier compile against it. `runLLMTurn`'s routing already
  exists (the renamed former `returnControl` path); WS-B adjusts it when it
  splits `runLLMTurn` out of `Ask`.
- Runtime driver interface (Rust) as stubs: the outer lifecycle enum (model on
  `tidepool-repl/src/state.rs:56-72`), and signatures for `run_loop`,
  `service_runllm_hole`, `state_out`/`state_in` (JSON crossing),
  `render_framing`, `compaction_trigger`.
- `render`/`loop` authored-Haskell type contract + a trivial example harness
  file (`examples/harness/{Harness.hs}` with `render`/`loop`/a concrete `State`)
  for tests.

**S4 — Commit scaffold; `cargo check --workspace` green with stubs.**

## Fork wave (parallel; each own worktree)

Each spec: **READ FIRST** (from recon) · **ANTI-PATTERNS** · **STEPS** · **VERIFY** · **DONE**.

### WS-A — Runtime driver spine  · **opus** (may sub-decompose: bootstrap / turn-servicing / lifecycle)
- READ: `tidepool-runtime/src/session/persistent.rs:173-219` (Threadless),
  `tidepool-harness/src/harness.rs:634-676,975-1016` (drive_turn/run loop),
  `resident.rs:407-465` (run_child/resume), `engine.rs:593-612` (drive_model_turn).
- ANTI-PATTERNS: don't reuse the repl parked-thread path (dead end once forking);
  don't reimplement the turn loop — reshape `drive_turn`/`run_to_hole_or_done`;
  don't copy heaps — `run_child` is zero-copy into the suspended parent.
- STEPS: bootstrap a `PersistentSession<Threadless>` for the Harness eval; load
  `render`/`loop` (via WS-D's bootstrap-load stub); run `loop state` as a
  suspendable fragment; on a `runLLMTurn @A` suspension, drive a nested Agent
  session (reuse the existing turn loop) to a `finalize` (WS-B) and `run_child`
  the value back to resume `loop`; alternate `render`→`loop`; own the outer
  lifecycle enum; emit lifecycle events to the WS-H `Observer`.
- VERIFY: `cargo nextest -p tidepool-harness --ignore-default-filter` new
  `selfharness_spine` test: a trivial `loop` with one `runLLMTurn` answered via
  `finalize` completes and re-renders.
- DONE: one full render→loop→runLLMTurn→finalize→render cycle runs end to end.

### WS-B — Shared suspend machinery + `RunLLMTurn` + `Finalize` effects  · **opus**
- READ: recon C (the runLLMTurn/former-returnControl mechanism end-to-end),
  `Translate.hs:1578-1663,2723-2762`, `effect_defs.rs:576-694` (Ask def),
  `effect_glue.rs:23-79`, `eval_prep.rs:43-69` (base_effects!/interposed Ask),
  `engine.rs:82-124`, `harness.rs:1596-1691`.
- ANTI-PATTERNS: **do NOT copy Ask's code — SHARE it.** Factor the suspend-as-data
  + payload-classify + resume machinery into ONE common path that `Ask`,
  `RunLLMTurn`, and `Finalize` all consume; don't apply the function-arrow
  rejection to `finalize`; `finalize` does NOT resume the Agent — it *terminates*
  the Agent turn-loop and hands the value UP to the `loop` hole.
- STEPS: (1) factor the shared suspend/classify/resume code out of `Ask`;
  (2) **split `runLLMTurn` OUT of `Ask` into its own interposed `RunLLMTurn`
  effect** consuming the shared code (the rename left it an Ask verb) — so
  `Harness = Eff '[RunLLMTurn]`; (3) add the `Finalize` effect the same way (own
  tag, shared code, `@a` capture with NO arrow check, in-heap value); (4) the
  `HoleRouting::Finalize` arm (+ adjust `RunLLMTurn` routing after the split);
  (5) the harness handler that ends the Agent session and yields `finalize`'s
  value to WS-A's `service_runllm_hole` via `run_child`.
- VERIFY: `runLLMTurn @A` (from a Harness `loop`) and `finalize @A x` (from an
  Agent turn) both work; an Agent turn returning a closure via `finalize` is
  received+applied Harness-side; grep confirms no duplicated Ask machinery.
- DONE: `Harness = Eff '[RunLLMTurn]`, `Agent = base + Ask + Finalize`, all three
  sharing one factored suspend/classify path; `finalize @A x` resolves the parent
  `runLLMTurn @A` hole.

### WS-C — Typed `State` threading  · **sonnet**
- READ: recon B §5 (`value_to_json` engine.rs:1267 / session.rs:900;
  `input_binding_source`/`json_to_haskell` `tidepool-mcp/src/eval_prep.rs:366-389`).
- ANTI-PATTERNS: don't invent a new bridge — outbound reuses `value_to_json`,
  inbound reuses the input-literal splice; don't assume `Aeson.Value` — target
  the author's `(ToJSON s, FromJSON s) => s`.
- STEPS: outbound = `value_to_json` on `loop`'s returned `State`; inbound = splice
  the prior State JSON as a literal and bind `state :: State` via `eitherDecode`;
  thread across the restart-reinject.
- VERIFY: State round-trips (`loop` returns a modified record, next `render` sees it).
- DONE: an author `State` record survives a loop boundary and a restart.

### WS-D — `render`-as-framing + bootstrap-load  · **sonnet**
- READ: `engine.rs:164-230` (`assemble_request`/`SYSTEM_FRAMING`),
  `harness.rs:614-623,746-751` (decl-plane load), `mod.rs:307-396` (`define_batch`).
- ANTI-PATTERNS: don't leave `SYSTEM_FRAMING` hardcoded — the per-loop system
  message is `render(state, lastCompaction)`; don't load `render`/`loop` per-turn
  — load once at bootstrap.
- STEPS: parameterize `assemble_request` to take render's output as the system
  message; add a bootstrap-time decl-load path (variant of `define_scoped`) that
  loads `render`/`loop`/`State` source once.
- VERIFY: changing `render`'s output changes the Agent's system prompt.
- DONE: `render`'s text is the Agent turns' system message; harness source loads at boot.

### WS-E — Compaction  · **sonnet**
- READ: `03-runtime.md` compaction section; `engine.rs` token accounting;
  `provider.rs:34-50` (usage).
- ANTI-PATTERNS: the *runtime* owns the ~80% trigger, never the loop; the compact
  turn emits `Text` (feeds render's `Maybe Text`), distinct from `State`.
- STEPS: watch context usage in the driver; at threshold force a "compact to text,
  target X" turn; thread its `Text` into the next `render`.
- VERIFY: a loop that overruns triggers a forced text-compaction fed to render.
- DONE: emergency compaction fires at threshold and the summary reaches `render`.

### WS-F — `fork` in Harness  · **DEFERRED to a later wave** (operator decision)
Not in this v1 wave. When it lands: reshape the existing fanout machinery
(`harness.rs:1154-1324`, `Fork.hs`, `engine.rs:623-645` build_list_value) into a
Harness-level `fork` verb — reuse the `run_child` primitive, rebuild the
orchestration (a `fork` target is a `Harness` computation, not a model-driven
child node).

### WS-H — Event-observer extension point  · **sonnet** (foundational — WS-A emits to it)
- READ: `tidepool-harness/src/forcing.rs` (existing `Event`/`NodeTree` log),
  `harness.rs:374` (flush_effects).
- ANTI-PATTERNS: don't hardwire logging or the GUI into the driver — emit to a
  generic `Observer`/`EventSink` trait with pluggable subscribers.
- STEPS: define an `Observer` trait the driver calls at each event (loop
  boundary, turn start/end, `runLLMTurn` hole, `finalize`, compaction trigger);
  ship a `LogObserver` v1 subscriber; leave stub seams for a future Datastar-GUI
  subscriber AND for reactive hooks (the distillation loop's future event
  reactions — this is the "observe and react to events" extension point).
- VERIFY: a run emits the full event sequence to the log subscriber.
- DONE: events flow through a pluggable `Observer`; logging is one subscriber;
  GUI + reactive seams stubbed.

### WS-G — Binary + provider wiring + acceptance  · **sonnet (opus for the acceptance judgment)** · integrates LAST
- READ: `tidepool-web/src/bin/tidepool-harness.rs:49-71` (provider select),
  `provider/oauth.rs`, `replay.rs`.
- STEPS: the `tidepool-selfharness` binary; provider select (OAuth default,
  `--replay`, optional API-key flag); a **generic-assistant** example harness
  (an observe/decide/act `loop` over an input, **no file-edit effects**) + an
  end-to-end acceptance test through the production path (render/loop answering
  `runLLMTurn`(s) via `finalize`, State surviving a loop boundary).
- DONE: `scripts/battery.sh` + a green `acceptance_selfharness` golden path
  driving the generic-assistant harness.

## Merge order + gates

1. **Scaffold (S1–S4)** — root, first (includes the rename; battery green).
2. **WS-B (finalize)** + **WS-H (observer)** — merge early (WS-A integrates against them).
3. **WS-A (driver spine)** — the foundation; `cargo check` + `selfharness_spine`.
4. **WS-C / WS-D / WS-E** — any order after WS-A; build + targeted test per fold.
5. **WS-G** — integrates last (binary + generic-assistant acceptance).
   (WS-F `fork` deferred to a later wave.)

**Verification (wave policy).** This environment hard-kills background processes
at ~380s, and a full-workspace GHC battery is *hours* (every test forks a GHC
extract, capped at 4 concurrent, incl. ~900s proptest suites; every
`Translate.hs`/`effect_defs.rs` change invalidates the compiled-artifact cache
→ cold recompiles). So the per-fold gate is **`cargo check --workspace` + a
TARGETED slice** — `scripts/battery.sh -p <crate> -E 'binary(<x>)'` (it forwards
`$@` to nextest and keeps the extract-env guard, so a narrowed run finishes in
minutes). **Do NOT gate on bare `scripts/battery.sh`** (full `--workspace`).
Full coverage runs deliberately — sharded per-crate or on a quiet box / CI (see
the test-infra follow-up). Confirm any failure reproduces on the pre-change base.
Parallel worktrees touch shared files (`effect_defs.rs`, `Translate.hs`,
`engine.rs`, `harness.rs`) — folds are serial with a build check between.

## Out of scope for this wave (deferred, per §06)

`fork` / subagent branching (deferred to a later wave); file-edit / write
effects (v1 Agent is read/reason only — may grow toward a coding agent later);
distillation-loop tooling (manual claude-code on the repo for now); per-context
effect sets (v1 Harness = `runLLMTurn` only, grown as needed; Agent = base + Ask
+ `finalize`); the Datastar-GUI observatory (v1 emits events to a pluggable
`Observer` wired to logging — GUI subscriber comes later); State-format
migration; live hot-reload.
