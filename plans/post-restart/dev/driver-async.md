# Dev spec: driver-async (block_in_place cleanup + F5 disposition)

Two independent pieces, both confined to files a sibling dev is not touching.

Source: `plans/post-restart/harness-lifecycle.md` §Scope items 3 (F5) and 4
(block_in_place cleanup). The block_in_place item is Inanna-endorsed.

## Boundary — read before touching anything

You own `src/selfharness/driver.rs` and `src/replay.rs`.

A sibling dev is concurrently rewriting `src/harness.rs` (session lifecycle
moves into `SessionRegistry`) and will make exactly ONE edit inside
`driver.rs`: the body of `retire_answerer` (~line 899). **Leave
`retire_answerer` alone** — do not move it, do not reindent it, do not change
the lines around it more than you must. Everything else in `driver.rs` is
yours.

You will need to touch the CALLERS of the driver's entry points (tests and
`tidepool-web/src/bin/tidepool-selfharness.rs`) — that is expected and in
scope.

Do NOT re-fix what the durability wave landed (see
`plans/self-iterating-harness/12-robustness-wave-receipt.md`): F3 lifecycle
states, F4 checkpoint, F6/F7 ordering, the turn lease. Those are DONE.

## Part 1 — `block_in_place` cleanup

`driver.rs` has ~8 live `block_in_place` sites. They are **two different
things**, and only one of them is jank:

**(a) Sync→async bridges — DELETE THESE.** The shape is
`tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(
self.agent.<async>()))`. Sites at roughly 1119, 1282, 1425, 1431, 1601
(re-locate; line numbers pre-date your branch). They exist for exactly one
reason: `SelfHarnessDriver`'s methods are `fn`, not `async fn`, so they cannot
`.await` the `Harness`. Every one of these disappears the moment the driver
is async.

**(b) Async→sync-blocking `OperatorGate` calls — KEEP THESE.**
`block_in_place(move || gate.present_form(&form))` and the
`gate.await_continue()` in `between_loops_gate` (~856, ~1281, ~1358).
`OperatorGate::present_form`/`await_continue` are SYNC-BLOCKING **by frozen
contract** (`selfharness/operator.rs`; a web/GUI gate parks on a channel
there). Calling a genuinely blocking function from an async fn is exactly
what `block_in_place` is for. These stay.

### What to do

1. Make `SelfHarnessDriver`'s turn-driving methods `async fn` and `.await` the
   `Harness` directly. The conversion is transitive: `run_one_cycle`,
   `run_loop`, `restore`, `run_loop_fragment`, `run_loop_fragment_inner`,
   `service_runllm_hole`, `drive_answerer_to_finalize`, `service_askuser_hole`,
   `service_outer_askuser_hole`, `drain_answerer_fork`,
   `maybe_compact_answerer`, `between_loops_gate` — whatever the call graph
   actually requires. Convert what needs it; do not async-ify pure accessors
   (`lifecycle`, `checkpoint_path`, `answer_contract`, the `set_*` setters).

2. Delete every category-(a) bridge, and its now-false comment with it.

3. Keep every category-(b) gate call under `block_in_place`, and **correct the
   comments**: they currently justify themselves by "the driver already runs
   its turn loop via `block_in_place`/`block_on`, not `async fn`" — which will
   no longer be true. Rewrite them to state what IS true: the gate is
   sync-blocking by contract, so a web gate's channel park must yield the
   tokio worker. Fix the same claim in `driver.rs`'s module doc (~26),
   `selfharness/operator.rs`'s module doc (~8), and
   `tidepool-harness/CLAUDE.md`'s operator-gate section (which says "the
   driver already runs its turn loop via `block_in_place`/`block_on`, not
   `async fn`").

4. Update the callers: `.await` at each `run_one_cycle` / `run_loop` /
   `restore` call site in `tidepool-harness/tests/*` and in
   `tidepool-web/src/bin/tidepool-selfharness.rs` (~151). Also fix that bin's
   module doc, which explains the multi-thread-runtime requirement in terms of
   `block_in_place` + `block_on`.

5. **Known non-goal — do not expand into it.** The resident JIT runs
   (`outer.session.run(...)`, `session.resume(...)`) are CPU-blocking calls
   that will now sit inside `async fn`s. They already block a tokio worker
   today (the sync `run_one_cycle` is called straight from async test bodies
   and from `#[tokio::main]` without any bridge), so making the enclosing fn
   async changes nothing about that. Do NOT try to `spawn_blocking` the
   resident session — it is not `Send`-shaped for that and it is a different
   piece of work. Say in your submit note that you left it, and why. The
   multi-thread runtime requirement is unchanged either way (the kept
   category-(b) `block_in_place` calls still require it).

## Part 2 — F5 disposition: `fold_tree_state` / `FoldedTree`

**The decision is made — implement it, do not re-litigate.**

`replay.rs`'s `fold_tree_state`/`FoldedTree` folds a durable log into the
terminal per-node `NodeState` + tree structure. It has zero production
consumers; its only callers are `replay.rs`'s own unit tests and
`tests/golden_path.rs`'s crash-replay round-trip assertion.

**Disposition: keep it, document it honestly as OFFLINE log inspection. Do
NOT wire it into startup.**

The reason is the landed generation-tagged `Checkpoint` (durability wave, F4):
the driver already restores its state from that checkpoint at startup. Folding
the log at startup as a second recovery source would install exactly the dual
lifecycle machinery this whole lane exists to remove — two mechanisms that can
disagree about what a run's state was. The checkpoint is the recovery
mechanism; the fold is an inspection tool over the durable log, in the same
family as the documented `tail -f log.jsonl` workflow.

It is also not dead code: `golden_path` asserts the real property (a killed
process's log folds back to the terminal tree) and that assertion is the
contract `replay.rs`'s module doc states.

### What to do

- Rewrite the doc comments on `fold_tree_state`, `FoldedTree`, and the
  relevant part of `replay.rs`'s module doc so they say what this IS: an
  offline read over a durable log, for inspecting a finished or crashed run's
  tree — explicitly NOT the startup recovery path, which is the checkpoint.
  Name the checkpoint so a reader knows where to look instead.
- No behavioral change, no rename unless the current name actively misleads
  (it does not — `fold_tree_state` is accurate).
- Fix `tidepool-harness/CLAUDE.md`'s Replay section if it implies otherwise.
- Comments describe what IS. No "formerly", no history narration — that goes
  in the commit message.

## Verify

Set up ONCE:

```
export PATH=/nix/store/i7xkw0wd599j23fbsz8ydmsfj4dp9831-ghc-native-bignum-9.12.2-with-packages/bin:$PATH
export TIDEPOOL_EXTRACT=/home/inanna/dev/tidepool/haskell/dist-newstyle/build/x86_64-linux/ghc-9.12.2/tidepool-extract-0.1.0.0/x/tidepool-extract-bin/build/tidepool-extract-bin/tidepool-extract-bin
```

That extract binary is SHARED and READ-ONLY. Never rebuild it, never touch
`haskell/`.

1. `cargo check --workspace --all-targets`, `cargo fmt --all -- --check`,
   `cargo clippy --workspace`. Three clippy warnings are pre-existing and not
   yours (tidepool-codegen `large_enum_variant`, `engine.rs` `TurnOutcome`
   `large_enum_variant`, `selfharness_compaction_fixes` `type_complexity`).
2. Quick tier: `cargo nextest run`. Report the tests-RUN count.
3. GHC-heavy, in shards sized under the ~380s process kill. Every GHC-heavy
   invocation goes through the slot script, absolute path:

   ```
   /home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- \
     cargo nextest run --ignore-default-filter -j1 -p tidepool-harness \
     -E 'binary(acceptance_selfharness) | binary(selfharness_spine)' \
     --no-fail-fast
   ```

   Cover, across shards: every binary whose test calls `run_one_cycle` /
   `run_loop` / `restore` (you changed each of their call sites) —
   `acceptance_selfharness`, `acceptance_askuser`, `acceptance_fork`,
   `selfharness_spine`, `selfharness_framing`, `selfharness_lifecycle`,
   `selfharness_persistence`, `selfharness_compaction_fixes`,
   `exact_context_fork` — plus `golden_path` (it is the F5 consumer) and
   `tidepool-web`'s `crash_recovery` (it spawns the REAL
   `tidepool-selfharness` binary you re-plumbed; it is GHC-heavy despite
   living in tidepool-web, so it needs a slot too).
   NOT `selfharness_compaction` (known open-intermittent, ~200s), and leave
   `TIDEPOOL_EXPENSIVE_TESTS` unset.

`crash_recovery` is the one that would catch a botched async conversion in the
real binary — do not skip it.

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
- Never `git add -A`. Never force-push. Repo-root `tmp/` is protected human
  scratch. Commit with `--no-verify` (the hooks run tests; standing directive).
- A flaky test never lands. Fix it, or narrow it to a documented
  non-property, repetition-gated 15+ runs.

## Done criteria

- Zero `block_in_place(|| Handle::current().block_on(..))` bridges remain in
  `driver.rs`.
- The `OperatorGate` `block_in_place` calls remain, with comments that state
  the real (sync-blocking-contract) reason rather than the stale
  "driver isn't async" one.
- `driver.rs` module doc, `operator.rs` module doc,
  `tidepool-web/src/bin/tidepool-selfharness.rs` module doc, and
  `tidepool-harness/CLAUDE.md` no longer claim the driver is non-async.
- F5 documented per the decision above; no startup wiring.
- check / fmt / clippy clean; quick tier + GHC-heavy shards reported with
  per-binary tests-RUN counts, including `crash_recovery`.
- Submit note states explicitly that the resident-JIT blocking calls were left
  in place, and why.
