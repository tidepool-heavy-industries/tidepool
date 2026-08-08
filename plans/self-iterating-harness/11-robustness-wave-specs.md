# Robustness wave — per-finding specs (F3, F4, F6, F7 + crash recovery)

Criteria of record: `10-external-review-findings.md`, "Post-recovery ROBUSTNESS
WAVE" section. This file restates each row as an implementable spec with
explicit file/function ownership, so the lanes below stay disjoint.

## Ownership map

| Lane | Files it may edit |
|------|-------------------|
| F3 | `selfharness/lifecycle.rs` (whole file), `selfharness/driver.rs` — `DriverError`, `bootstrap`, `run_one_cycle`, `run_loop_fragment`, `retire_answerer`, new guard helpers; `tests/selfharness_lifecycle.rs` (new) |
| F4 | `selfharness/persistence.rs` (whole file), `selfharness/driver.rs` — `restore`, `run_loop`, `set_last_compaction`, the `state_path`/`compaction_path` fields + accessors; `selfharness/harness_source.rs` (fingerprint only); `tests/selfharness_persistence.rs` |
| F6+F7 | `harness.rs` only |
| Crash recovery | `tidepool-web/tests/` (new file), `.config/nextest.toml` (filter rows only) |

Nobody edits: `tidepool-mcp/src/effect_defs.rs`, `tidepool-mcp/src/preamble.rs`,
`engine.rs`'s `finalize_shim` region, `driver.rs`'s `service_runllm_hole` /
`AnswerContract` region, `compile.rs` timing.

---

## F3 — lifecycle carries failure; error exits discard resident state

**Criterion (findings doc):** "unconditional `Idle` is the cosmetic half —
needs `Failed`/`Poisoned` or an error guard that restores/discards every
mutable resident component before publishing `Idle`."

Today `run_one_cycle` sets `self.lifecycle = SelfHarnessState::Idle`
unconditionally after the fallible body. The advertised state is then a lie in
two ways: the driver reports "idle, ready for a cycle" after a failure, and the
outer resident session may be left mid-fragment (suspended on a hole) with
`answerer_framing`/`cycle_compaction`/`loop_inference_calls` carrying the failed
cycle's residue into the next one.

Deliver:

1. `SelfHarnessState::Failed { reason: String }` — a cycle errored; the driver
   discarded the failed cycle's resident state and the NEXT cycle re-bootstraps
   cleanly. Recoverable.
2. `SelfHarnessState::Poisoned { reason: String }` — the discard itself could
   not complete (the outer session could not be dropped/rebuilt). Every public
   entry point (`run_one_cycle`, `run_loop`, `restore`) returns
   `DriverError::Poisoned` instead of running.
3. An error guard on `run_one_cycle`'s fallible body that, before publishing a
   terminal lifecycle state, discards every mutable resident component:
   `answerer` (retire), `answerer_framing`, `cycle_compaction`,
   `loop_inference_calls`, and — critically — `self.outer`, whose session may be
   parked mid-fragment. Dropping `outer` is the discard: `bootstrap` rebuilds it
   from `source` on the next cycle (it is a no-op only while `outer` is `Some`,
   so a `None` after a failure re-bootstraps naturally).
4. `label()` covers both new variants; `is_idle()` stays false for them.
5. `DriverError::Poisoned(String)`.

Acceptance (`tests/selfharness_lifecycle.rs`, no GHC needed if a failure can be
induced without one — otherwise gate on `TIDEPOOL_EXTRACT` like its siblings):

- a cycle that errors leaves `lifecycle()` as `Failed`, not `Idle`;
- after that failure `self.outer` is discarded, so the next `run_one_cycle`
  re-bootstraps rather than running against a parked session (assert via a
  successful cycle following a failed one);
- `Poisoned` refuses subsequent entry points with `DriverError::Poisoned`.

Do NOT consolidate the four state machines (`NodeTree` state ↔
`NodeConvo.pending` ↔ `ResidentSession` ↔ durable-log fold). That is a separate
wave. This lane changes the OUTER driver lifecycle only.

---

## F4 — one generation-tagged checkpoint

**Criterion (findings doc):** "state + compaction = ONE generation-tagged
checkpoint. Acceptance: after a crash at every write boundary, restart selects a
state + summary from the SAME committed generation + harness source."

Today `state.json` is written at cycle end (`run_loop`) and `compaction.txt`
mid-loop (`set_last_compaction`). A crash between them restarts with generation
N's state and generation N+1's summary — a summary of work that was rolled back.

Deliver a single `Checkpoint` record, written atomically (tmp + rename) to one
file:

```rust
struct Checkpoint {
    generation: u64,          // monotonic, +1 per committed cycle
    state: Json,              // `loop`'s returned State for that generation
    compaction: Option<String>,   // the summary in force AT that generation
    harness_source: String,   // fingerprint of the source that produced it
}
```

Rules:

- **One writer, one boundary.** The checkpoint commits at cycle end only, with
  the cycle's state and the compaction summary in force at that moment. A
  mid-loop compaction updates `self.last_compaction` in memory (the loop already
  continues under it) but does NOT commit on its own — so a crash mid-loop
  restarts from generation N's state AND generation N's summary, never a mixed
  pair. `set_last_compaction` loses its persist call and becomes in-memory only.
- **Atomic.** `.tmp` sibling + rename, as `save_state` does today. A kill at any
  point leaves either generation N or N+1 fully readable, never a torn file.
- **Fingerprint.** `harness_source` is a content hash of the loaded harness
  source (add a `fingerprint()` to `HarnessSource` — hash of the source text is
  enough; do not invent a manifest format). On restore, a MISMATCH does not
  refuse to start (the harness file is expected to change during dogfood — that
  is the point of self-iteration): it restores the state and emits a loud
  observer `Event` naming both fingerprints, so a subsequent `StateDecode`
  failure is diagnosable instead of mysterious.
- **Missing file is `Ok(None)`**, as `load_state` is today — the first-ever run.
- **Malformed file is an error**, not a silent reset.

Driver surface: `restore()` keeps its signature (`Result<Option<Json>,
DriverError>`) and sets `self.last_compaction` from the same record.
`state_path`/`compaction_path` collapse to one `checkpoint_path` +
`set_checkpoint_path`/`checkpoint_path` accessors; update the call sites and
`tests/selfharness_persistence.rs`. `persistence::default_state_path` /
`default_compaction_path` / `save_state` / `load_state` / `save_compaction` /
`load_compaction` go away with them — this is a replacement, not a second
mechanism alongside the old one. Leave `default_transcript_path` /
`default_log_path` / `JsonlObserver` alone.

Acceptance (extend `tests/selfharness_persistence.rs`):

- a mid-loop compaction followed by a crash BEFORE the cycle commits restores
  the prior generation's state AND the prior generation's summary (not the new
  summary against the old state);
- a committed cycle restores state and summary from the same generation;
- generation increases monotonically across cycles;
- a truncated/torn checkpoint file is a typed error, and the atomic write never
  produces one (no `.tmp` left behind).

---

## F6 — `flush_effects` must not advance `effect_seq` past a failed append

**Criterion (findings doc):** "`flush_effects` drains then discards write
failures + advances `effect_seq` — don't advance past a failed append; fail the
turn or queue for retry."

`Harness::flush_effects` (`harness.rs`) `mem::take`s the node's effect trace,
then `let _ = self.tree.effect(...)` per record and unconditionally stores the
advanced `seq`. A failed durable append therefore loses the request/response
pair AND burns its sequence number — a hole in the audit log that nothing
reports.

Deliver: the durable log is the audit contract, so a failed append **fails the
turn** and loses nothing.

- `flush_effects` returns `Result<(), HarnessError>`.
- On the first append error: stop, do NOT advance `effect_seq` past the last
  SUCCESSFUL append, restore the unwritten records (in original order, ahead of
  anything a concurrent path has since pushed) into the node's `effect_trace`,
  and return the error.
- Call sites propagate with `?`.

Choose ONE mechanism — fail-the-turn — not fail-and-also-silently-retry.

Acceptance: a unit test with a `LogWriter` whose append fails (closed/read-only
target, or a seam that lets the test inject the failure) shows (a) the turn
errors, (b) `effect_seq` did not advance past the failure, (c) the unwritten
records are still in the node's trace.

---

## F7 — a per-node turn lease

**Criterion (findings doc):** "per-node turn race: a turn LEASE must cover
snapshot → provider await → log append → resident run → outcome publish
(broader than `SessionRegistry`)."

`drive_turn` snapshots `(transcript, turn_seq, framing)` under the `convos`
lock, RELEASES it, then awaits the provider. Two concurrent turns on one node
both read `turn_seq = N`, both log turn N, and both append to the transcript in
nondeterministic order.

Deliver an RAII lease on `NodeConvo`:

- a `turn_lease: bool` (or `Option<LeaseId>`) field on `NodeConvo`;
- `acquire_turn_lease(&self, node) -> Result<TurnLease<'_>, HarnessError>` —
  sets the flag under the `convos` lock if clear, else returns a new
  `HarnessError::TurnInFlight(NodeId)`;
- `TurnLease`'s `Drop` clears the flag, so every exit path (success, `?`, panic
  unwind) releases it.

Acquire at exactly ONE layer, or a nested acquire deadlocks the node against
itself. Acquire in the leaf orchestration entry points that own a whole turn:
`drive_turn`, `summarize_turn`, and each `answer_*` method that compiles + runs
+ publishes. Do NOT acquire in `run_to_hole_or_done` (it loops `drive_turn`).
Audit the call graph before wiring — an accidental double-acquire on one node is
the failure mode to avoid, and the lease covers the WHOLE span, provider await
included.

Acceptance: two concurrent `drive_turn` calls on the same node yield exactly one
success and one `TurnInFlight`; `turn_seq` advances by exactly one; a turn that
returns `Err` still releases the lease (a following turn on that node succeeds).

---

## Crash-recovery acceptance

Drives the REAL entry point under natural conditions — no hand-wired harness.

- Spawn the real `tidepool-selfharness` binary
  (`env!("CARGO_BIN_EXE_tidepool-selfharness")`, so the test lives in
  `tidepool-web/tests/`) in `--replay <log> ` mode, which implies `--auto` (no
  operator gate) and needs no API key.
- Isolate its durable state with `XDG_CACHE_HOME` pointed at the test's temp dir
  — the binary resolves every path through `tidepool_runtime::paths::cache_dir`.
- Let it get INTO an answerer turn (watch the durable log / transcript jsonl for
  the marker), then `SIGKILL` it. Kill only the PID this test spawned — never a
  pattern match.
- Restart the same binary against the same cache dir and let it run to
  completion of the replay script.
- Assert: the restarted process resumes from the persisted checkpoint (its first
  cycle continues the prior state rather than restarting from `initialState`),
  the loop completes, and the checkpoint file parses cleanly at every observed
  point (never torn).

Assert through the driver's PUBLIC surface and the binary's behavior — do not
hard-code a checkpoint FILENAME or its internal JSON shape; F4 owns that format
and it is changing in this same wave.

Register the new binary in `.config/nextest.toml` (both the `default-filter`
exclusion and the `ghc-heavy` override filter) — it drives real GHC extracts.
