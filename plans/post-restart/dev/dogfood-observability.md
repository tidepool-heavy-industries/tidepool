# Dev spec: dogfood-observability (wave 1.5)

Give the self-iterating harness a SEMANTIC log — console (INFO) and durable
jsonl — so a person watching a live dogfood run can narrate what the harness is
doing without reading code, and so tier-0 telemetry is a fold over a file
rather than a guess.

Driven by live dogfood feedback (Inanna, driving the wizard harness): *"logging
is bad … the timing, sure, but I want to see the extracted types, the string
the llm generates for each compile run, etc."*

## The observed reality this fixes

On a live run: the run `log.jsonl` held ONE header line, the transcript held
two events, and the console showed only `timing` DEBUG lines rendering
`node=18446744073709551615 round=18446744073709551615` — `u64::MAX` sentinels
printed raw.

Some of that emptiness was the boot crash (the process died during the first
`render`, before any turn). **Do not assume the rest is broken — measure it.**
Phase 0 below is that measurement, and its result is part of your receipt.

## Logging idiom — read this before you write a macro

This crate logs through **`tracing`**, not the `log` facade: `timing.rs` uses
`tracing::debug!`, `observer::LogObserver` uses `tracing::info!`, and the
binary installs a `tracing_subscriber` with `EnvFilter`. The standing repo rule
is "structured logging, never `eprintln!`-added-then-stripped" — in
`tidepool-harness` that means `tracing::` macros. Use them. Do not introduce
the `log` crate here, and do not add a single `eprintln!`.

`harness-dogfooding/run.sh` sets `RUST_LOG=warn,tidepool_harness=debug,…`, so
`INFO` from this crate is default-visible in a dogfood run. That is the tier
the semantic layer lives at. **The existing `timing` DEBUG lines stay exactly
as they are** — they are the latency wave's instrumentation, and this work adds
a layer above them, it does not replace or relocate them.

## Two event surfaces — know which one you are adding to

- `crate::log::Event` (`log/`, written by `NodeTree` via `LogWriter`) — the
  durable PER-NODE log, `<cache>/selfharness/log.jsonl`. Already carries
  `TurnStart{source}` with the extracted executed Haskell block,
  `TurnDelta`, `HolePublished`/`HoleConsumed`, `Effect`, `NodeDone`.
- `crate::selfharness::observer::Event` (`selfharness/observer.rs`) — the
  driver's LOOP-level transcript, `<cache>/selfharness/transcript.jsonl` via
  `JsonlObserver`, and the console via `LogObserver`. Currently:
  `LoopBoundary`, `TurnStart`, `TurnEnd`, `RunLLMTurnHole`, `Finalize`,
  `CompactionTrigger`, `HarnessSourceChanged`.

`tidepool-harness/CLAUDE.md`'s "Tailing the durable log" section documents both
and is the thing you must keep true.

## Phase 0 — inventory first (this is work, not preamble)

Drive one healthy cycle through the replay provider (the acceptance path —
`SelfHarnessDriver::run_one_cycle` with a `ReplayProvider`, as
`acceptance_selfharness` does) and record, verbatim:

- every line that reaches the console at the dogfood's `RUST_LOG` level;
- every event that lands in `transcript.jsonl`;
- every event that lands in `log.jsonl`.

That inventory is the BASELINE. Put it in your submit note. Everything below is
"close the gap between this inventory and the deliverables" — if the inventory
shows something already present, say so and do not rebuild it.

## Deliverables

### 1. The LLM-generated Haskell source, verbatim, per compile

Every compile that happens on a run must be visible as the exact source string
compiled.

- Agent/answerer turns: `crate::log::Event::TurnStart{source}` already carries
  the extracted executed block into `log.jsonl`. What is missing is the
  console — surface it at INFO.
- The OUTER loop's compiles are the real gap: `compile_outer` (`driver.rs`
  ~617) compiles `render`, `Loaded.loop __selfHarnessState`, and the
  `state_cross` helper splice, and there is no per-node log for the outer
  session at all. Add driver-level events carrying the compiled source.

Long sources: log them in full to jsonl (it is a durable record and truncation
there destroys the artifact). For the console, full text is what Inanna asked
for — do not truncate. If a source is pathologically large, that is itself
signal worth seeing.

### 2. Extracted types per turn

What extract said the holes and binds ARE:

- the asks sidecar's site → type table (`AsksSidecar`, `compile.rs`) for each
  compiled turn;
- the bound-binder types on the value-plane bind path (`BoundBinder`).

Neither is logged today. Both to console INFO and to jsonl, keyed to the turn
they came from.

### 3. Compile/extract errors, verbatim, with the retry visible

A failing turn currently feeds the model a GHC error truncated to 3000 chars
(`truncate_ghc_error`, `harness.rs` ~305) and the retry loop is otherwise
silent. Log the **untruncated** error, and log each corrective-retry round with
its round index — a retry loop that burns rounds must be visible while it is
happening, not reconstructable afterwards. The truncation for the MODEL stays
as it is; this is about the log.

### 4. Effect yields / ask contents

As each hole fires: which hole, what prompt, and what answer came back.
Covers `ask`, `askUser` (the operator form spec AND the submission that came
back), and `runLLMTurn`/`finalize` hole traffic. `HolePublished`/`HoleConsumed`
exist in the durable per-node log; the loop-level transcript and the console
have nothing usable.

### 5. Sentinel rendering

`timing.rs` defines `NO_ROUND` / `NO_NODE` as `u64::MAX` (~62, ~69) and they
render raw as `18446744073709551615`. Render them as `bootstrap` (no node) and
`-` (no round) — or equivalents that read as words, not numbers. Fix it at the
rendering site so every emitter benefits; do not patch call sites one by one.

## jsonl completeness — the acceptance that matters

Tier-0 telemetry is a FOLD OVER `transcript.jsonl`. An event that is not
recorded is a metric that does not exist. Two metrics must be computable from
the transcript alone, with no other input:

- **first-compile success rate** — of the turns that compiled, what fraction
  succeeded on the first attempt;
- **retries-per-hole** — how many corrective rounds each hole consumed.

Design the events so those folds are possible (they need, at minimum: a hole
identity, a per-turn attempt index, and a compile outcome). **Write the folds
as a test.** A test that computes both metrics from a replayed cycle's
transcript file is the real proof the events are complete — it fails the moment
someone adds a path that does not emit.

## Acceptance

1. **The narration test.** Capture the tracing output of one replayed cycle and
   assert the narration elements are present: the compiled source, the
   extracted types, each hole's prompt, each answer. Assert `u64::MAX` never
   appears in the captured output.
2. **The telemetry fold test.** Compute first-compile success rate and
   retries-per-hole from `transcript.jsonl` alone, per above.
3. **Mutation-close at least the fold test**: remove one emit site and the
   fold must go wrong (a changed count, not a silent pass). Report the exact
   assertion message the mutant produced.
4. Keep `tidepool-harness/CLAUDE.md`'s "Tailing the durable log" section true —
   it enumerates what each stream carries, and you are changing that.

The human criterion, and the one to design against: *a person watching the
console can narrate what the harness is doing — what source it compiled, what
types it extracted, what it asked, what came back — without reading code.*

## Verify

Set up ONCE:

```
export PATH=/nix/store/i7xkw0wd599j23fbsz8ydmsfj4dp9831-ghc-native-bignum-9.12.2-with-packages/bin:$PATH
export TIDEPOOL_EXTRACT=/home/inanna/dev/tidepool/haskell/dist-newstyle/build/x86_64-linux/ghc-9.12.2/tidepool-extract-0.1.0.0/x/tidepool-extract-bin/build/tidepool-extract-bin/tidepool-extract-bin
```

Shared and READ-ONLY. Never rebuild it, never touch `haskell/`.

1. `cargo check --workspace --all-targets`, `cargo fmt --all -- --check`,
   `cargo clippy --workspace`. Three clippy warnings are pre-existing and not
   yours (tidepool-codegen `large_enum_variant`, `engine.rs` `TurnOutcome`
   `large_enum_variant`, `selfharness_compaction_fixes` `type_complexity`).
2. Quick tier: `cargo nextest run`. Report the tests-RUN count.
3. GHC-heavy, in shards under the ~380s process kill, each through the slot
   script at its absolute path:

   ```
   /home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- \
     cargo nextest run --ignore-default-filter -j1 -p tidepool-harness \
     -E 'binary(acceptance_selfharness) | binary(selfharness_spine)' \
     --no-fail-fast
   ```

   Cover `acceptance_selfharness`, `selfharness_spine`, `selfharness_framing`,
   `selfharness_persistence`, `selfharness_lifecycle`, `acceptance_askuser`,
   plus whichever binary holds your new tests. NOT `selfharness_compaction`
   (known open-intermittent, ~200s); leave `TIDEPOOL_EXPENSIVE_TESTS` unset.

## Contention rules — verbatim, non-negotiable

- Every GHC-heavy run goes through
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>` (absolute
  path). NEVER `exclusive` mode. `.config/nextest.toml`'s `ghc-heavy` group is
  default-deny and caps concurrent extract compiles — do not override it.
- No LSP / rust-analyzer. `grep` and `Read` only. A per-worktree
  rust-analyzer is 3-5 GiB and this box is shared.
- Scope every kill to your OWN PID or your OWN worktree path. NEVER a bare
  `pkill -f <pattern>` — those patterns match other agents' prompts and kill
  sibling worktrees' processes.
- `--no-fail-fast` on any suite with a known red. Gate on tests-RUN counts,
  never on exit codes. Capture full output to a file and extract afterwards —
  never pipe through `head`/`tail` at capture time.
- **Run GHC-heavy binaries DETACHED, not foreground.** This environment
  hard-kills background processes at ~380s. Measured on this lane:
  `selfharness_lifecycle` needs ~596s and `acceptance_selfharness` ~210s, so
  a foreground `selfharness_lifecycle` WILL be killed mid-suite and is
  indistinguishable from a failure by exit status. A short tests-RUN count
  means RE-RUN, not "failure".
- Never `git add -A`. Never force-push. Repo-root `tmp/` is protected human
  scratch. Commit with `--no-verify` (the hooks run tests; standing directive).
- A flaky test never lands. Fix it, or narrow it to a documented
  non-property, repetition-gated 15+ runs.

## Boundary

- `tracing::`, never `eprintln!`, never the `log` facade in this crate.
- The `timing` DEBUG instrumentation stays; you add a layer above it. The one
  exception is deliverable 5 — the sentinel RENDERING.
- Do not change effect semantics, the turn loop, or the hole-classification
  path to make logging easier. If a value you need is not reachable from an
  emit site, say so in the submit note rather than restructuring the turn loop
  around it.
- Comments describe what IS. History goes in the commit message.

## Done criteria

- All five deliverables landed, each traceable to a line in your Phase-0
  inventory (either "was missing, added here" or "already present, surfaced at
  INFO").
- Both acceptance tests green; the fold test mutation-closed with its
  mutant-red receipt.
- `u64::MAX` appears nowhere in a dogfood run's console output.
- `tidepool-harness/CLAUDE.md`'s durable-log section true.
- check / fmt / clippy clean; quick tier + GHC-heavy shards reported with
  per-binary tests-RUN counts.
