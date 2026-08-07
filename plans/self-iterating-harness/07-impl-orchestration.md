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
- **`finalize` wiring:** a new verb/helper inside the existing `Ask` effect def,
  reusing the `AskWith` constructor with a `"finalize"` payload discriminant +
  a new `HoleRouting::Finalize` arm — **not** a new union-tag effect (avoids a
  new positional stack slot; consistent with how `dialogAsk`/`returnControl*`
  already share `AskWith`).

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
- Harness effect stack decl: `type Harness = Eff '[...]` with `runLLMTurn`,
  `finalize`, `fork` verbs (extend `effect_defs.rs`; `runLLMTurn` = renamed
  `returnControl`).
- `finalize`: `effect_defs.rs` helper stub + `Translate.hs` detection stub
  (mirror `isReturnControlVar`, capture `@a` via the existing `[Type ty] <- typeArgs`
  idiom, **skip the `typeHasFunctionArrow` rejection**) + `"finalize"` wire
  discriminant + `HoleRouting::Finalize { value }` variant in
  `engine.rs:82-124` + a `classify_hole` arm stub.
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
  lifecycle enum.
- VERIFY: `cargo nextest -p tidepool-harness --ignore-default-filter` new
  `selfharness_spine` test: a trivial `loop` with one `runLLMTurn` answered via
  `finalize` completes and re-renders.
- DONE: one full render→loop→runLLMTurn→finalize→render cycle runs end to end.

### WS-B — `finalize` effect  · **opus** (cross-layer extract + harness handler)
- READ: recon C (returnControl end-to-end), `Translate.hs:1578-1663,2723-2762`,
  `effect_defs.rs:620-690`, `engine.rs:82-124`, `harness.rs:1596-1691`.
- ANTI-PATTERNS: don't add a new union-tag effect (reuse `AskWith`); don't apply
  the function-arrow rejection to `finalize`; don't resume the Agent — `finalize`
  *terminates* the Agent turn-loop and hands the value UP to the `loop` hole.
- STEPS: fill the S3 finalize stubs — the `finalizeSited` body building
  `AskWith p {finalize:true, typedSite}`, the `Translate` detection + `@a`
  capture (no arrow check), the `HoleRouting::Finalize` classify arm, and the
  harness handler that ends the Agent session and yields the value to WS-A's
  `service_runllm_hole`.
- VERIFY: a test where an Agent turn returns a closure via `finalize` and the
  Harness side receives+applies it.
- DONE: `finalize @A x` inside an Agent turn resolves the parent `runLLMTurn @A` hole.

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

### WS-F — `fork` in Harness  · **sonnet**
- READ: `harness.rs:1154-1324` (answer_fork/answer_fanout), `Fork.hs`,
  `effect_defs.rs:647-666`, `engine.rs:623-645` (build_list_value).
- ANTI-PATTERNS: don't spawn model-driven child *nodes* with their own chat loop
  — a `fork` target is a `Harness` computation; reuse the `run_child` primitive,
  rebuild the orchestration.
- STEPS: expose `fork` as a Harness verb; split the agent session into logical
  threads off one context window; reassemble results.
- VERIFY: a `loop` forking 2 sub-computations gathers both.
- DONE: `fork` runs N Harness threads off one window and joins.

### WS-G — Binary + provider wiring + acceptance  · **sonnet (opus for the acceptance judgment)** · integrates LAST
- READ: `tidepool-web/src/bin/tidepool-harness.rs:49-71` (provider select),
  `provider/oauth.rs`, `replay.rs`.
- STEPS: the `tidepool-selfharness` binary; provider select (OAuth default,
  `--replay`, optional API-key flag); an end-to-end acceptance test through the
  production path (a real render/loop run answering one `runLLMTurn` via `finalize`).
- DONE: `scripts/battery.sh` + a green `acceptance_selfharness` golden path.

## Merge order + gates

1. **Scaffold (S1–S4)** — root, first (includes the rename; battery green).
2. **WS-B (finalize)** — merge early (WS-A integrates against it, not the stub).
3. **WS-A (driver spine)** — the foundation; `cargo check` + `selfharness_spine`.
4. **WS-C / WS-D / WS-E / WS-F** — any order after WS-A; build + targeted test per fold.
5. **WS-G** — integrates last (binary + acceptance through the production path).

Gate after every fold: `cargo check --workspace` + targeted nextest over touched
crates; `scripts/battery.sh` at the scaffold and at the final WS-G merge.
Parallel worktrees touch shared files (`effect_defs.rs`, `Translate.hs`,
`engine.rs`, `harness.rs`) — folds are serial with a build check between.

## Out of scope for this wave (deferred, per §06)

Distillation-loop tooling (manual claude-code on the repo for now), per-context
effect sets (v1 = the fixed full stack + Ask), the Datastar observatory as more
than an optional debug view, State-format migration, live hot-reload.
