# External review findings (2026-08-07) — triage + routing

An external frontier-model review (read-only, no edits/tests) of the
self-iterating harness. Triaged below into: **[FIXED]** done now,
**[FOLD→tl]** folded into a running recovery TL's in-scope work,
**[FOLLOW-ON]** a pre-existing robustness issue for a post-recovery wave (these
touch `harness.rs`/`forcing`/`replay`, being reconstructed now — not safely
editable), and **[DESIGN]** a question for the human. File:line are the
reviewer's, on the pre-crash tree.

## Self-harness path review

1. **[FOLD→root-integration] HIGH — answerer include-path not wired in the bin.**
   `tidepool-web/src/bin/tidepool-selfharness.rs:41` builds the answerer config
   with only `.tidepool/lib`; the loaded `--harness` dir (`source.source_dir`)
   is added to the OUTER session but NOT the nested answerer → `import
   HarnessTypes` fails under the real binary (incl. the dogfood wizard). Tests
   mask it by passing `examples/harness` as project_lib. Fix at root when wiring
   the real driver↔gate into the production bin.
2. **[DESIGN] HIGH — outbound `State` doesn't call the author's `ToJSON`.**
   `state_cross.rs:62` uses `value_to_json` (structural renderer), not Haskell
   `toJSON`; inbound uses `eitherDecode` (real `FromJSON`). Asymmetric protocol.
   Generic records happen to match, so NOT dogfood-blocking, but custom/tagged/
   renamed/versioned instances disagree, and the `StateDecode` error misdiagnoses.
   Needs a Haskell-side encode step — a protocol change; escalated.
3. **[FOLD→core-tl2] MED-HIGH — contradictory completion instruction.**
   The answerer's hole card (`engine.rs:336`, via `driver.rs:812`) says "evaluate
   `resume expr`", but the scoped `[AskUser, Finalize]` answerer's only valid
   commit is `finalize @T` — causes needless compile-error/retry rounds. Give the
   self-harness answerer a scoped hole card that says `finalize`.
4. **[FIXED] MED-HIGH — OperatorGate async/sync contradiction.** Plan locked
   async; scaffold `operator.rs` is sync-blocking (the considered decision,
   matching the stdin gate). Resolved the *plan* to sync + a `block_in_place`
   note (this file's sibling `09` updated).
5. **[FOLLOW-ON] MED — state+compaction are two files, one checkpoint.**
   Compaction persists mid-loop (`driver.rs:1119`); state only at cycle end
   (`:636`). A crash between → restart with old State + a summary of aborted work.
   Needs a combined/generation-tagged checkpoint. (Restart-safety wave.)
6. **[FOLD→core-tl2 + root] MED — production wires `LogObserver`, not `JsonlObserver`.**
   `bin:82` installs only the stderr logger; the loop lifecycle isn't durably
   recorded. Wire `JsonlObserver` (the bin wiring is root/gui; the WS4 story is
   core-tl2's).
7. **[FOLD→core-tl2] MED — error paths leave lifecycle stale.** `RunningLoop`/
   `Compacting` reset to `Idle` only on full success (`driver.rs:550`); errors
   leave a stale advertised state. Reset on error.
8. **[FOLLOW-ON] MED-LOW — driver silently bound to first HarnessSource.**
   `bootstrap` is a no-op once `outer` exists; a changed `HarnessSource` per call
   is silently ignored. Make source constructor-owned or reject a changed source.
9. **[FOLD→core-tl2] MED-LOW — historical-narration cruft.** `driver.rs` buries
   present-tense invariants under W1/W2/WS review history + an abandoned-mechanism
   diary + a duplicated finalize/compaction sentence; `selfharness/mod.rs:10`
   still calls the package a "scaffold full of `unimplemented!()`". De-bury the
   operative invariants + fix the stale scaffold description. (Matches
   comments-describe-what-is.)
10. **[FOLD→core-tl2] LOW — StdinGate swallows EOF/malformed input.**
    `operator.rs:77` `unwrap_or_default` makes EOF, bad JSON, and an empty form
    indistinguishable; `await_continue` ignores EOF → loses clean-shutdown.

## Broader harness/forcing/replay review — mostly [FOLLOW-ON]

Pre-existing robustness issues in the shared harness core (NOT this wave's
scope; a post-recovery **robustness/consolidation wave**):

- **HIGH** per-node turns not serialized around the provider call
  (`harness.rs:695` snapshots + releases the lock before awaiting the model →
  two concurrent calls race turn_seq/transcript). The unused `SessionRegistry`
  implements exactly the atomic `Idle→Running` this needs.
- **HIGH** effect-log write failures silently drop the request/response pair
  (`harness.rs:398` `mem::take` then ignores `tree.effect(...)` errors) — against
  the audit/replay contract.
- **HIGH** failed parent-resume can leave the three state machines disagreeing
  (`harness.rs:2205` restores the session before checking the resume result).
- **[FOLD→fork-tl2] HIGH** fork/fanout errors leak partially-live child state
  (`harness.rs:1540`/`1605` clean up only on full success; provider/join/log
  errors escape via `?` leaving the child Running + resident). → fork-tl2 lane.
- **[FOLD→core-tl2] MED-HIGH** unknown suspension constructor → silent empty
  `Ask` fallback (`engine.rs:116` `decode_askwith`) instead of a loud error;
  masks ABI drift / new constructors. core-tl2 is rewriting `classify_hole` →
  add the loud-unknown arm.
- **[FOLD→fork-tl2] MED-HIGH** fanout cardinality trusts inconsistent payload
  (`harness.rs:1616` ignores the `fan` count, one child per surviving prompt,
  silently drops non-string prompts, empty list → resumes with `[]`). Needs one
  authoritative cardinality + validate all elements decoded. → fork-tl2 lane.
- **[FOLD→core-tl2] MED-HIGH** `TurnStart` records `source:"model"` not the
  extracted executed Haskell; the answerer path emits no `TurnStart` at all →
  "tail the logs to see executed Haskell" is impossible today. This IS core-tl2's
  WS4 — WS4 must actually record the source + emit on the answerer path.
- **FOLLOW-ON** replay gaps: forked-child transcript prefix not materialized
  (`replay.rs:227`); `RecordedReply` node/turn recorded then ignored (`:98`);
  replay headers (prelude/extractor/version) read then discarded; `Done→Running`
  reopen unlogged (`forcing.rs:487`) so a crash mid-follow-up folds as `Done`.
- **MED** durable writer fsyncs all tree activity under one mutex (`forcing`) —
  unrelated nodes contend on disk latency.
- **[VERIFIED — non-issue]** default OAuth provider omits `max_tokens`
  (`provider/oauth.rs:578`) — DELIBERATE + documented: the ChatGPT Codex endpoint
  rejects `max_output_tokens` (`400 Unsupported parameter`); the real Codex CLI
  never sends it. Not a bug. The runaway-cost concern for the indefinite loop is
  real but already bounded by `LOOP_INFERENCE_CALL_CAP` (1024); a per-token cap
  on the OAuth route would need a client-side counter, not the rejected param.
  (Checked directly, provider/ is disjoint from all recovery TLs.)
- **MED-LOW** declaration compile/type errors bypass the self-correction loop
  (`harness.rs:839` returns `Resident`, not model-correctable `Compile`).
- **MED-LOW** cross-turn decl persistence silently degrades to `None` on setup
  error (`harness.rs:669`) → a later reference fails far from the cause.
- **MED-LOW** a `spawn_blocking` panic permanently loses the `Session`
  (ownership gone, `put_session` can't run) → `NodeConvo` with `session: None`,
  no explicit poisoned state.

## The architectural throughline (matches my own fresh-eyes pass)

Four overlapping state machines — `NodeTree` state ↔ `NodeConvo.pending`/
`session: Option` ↔ `ResidentSession` idle/suspended ↔ durable-log fold — where
the happy path updates all four but error/concurrent paths update one or two.
The unused `SessionRegistry` already models the cleaner explicit lifecycle
(`Idle | Running | RunningChild | Suspended`); the live path re-implements a
subset by convention, so comments carry invariants the types no longer enforce.
**The highest-value cleanup: one transactional orchestration boundary for
`reserve turn → call provider → log reply → run session → publish outcome`, and
one for `validate answer → run child → resume continuation → consume hole`, with
guard/cleanup objects so a mid-sequence failure can't leave a half-live node.**
This is the post-recovery consolidation wave; see [[no-scar-tissue]] and the
banked dead-`SessionRegistry` cleanup.

---

# Codex pass #2 (2026-08-07) — status reconciliation

A second read-only external review (`codex-review.md.tmp`). Its fair meta-point:
the statuses above read as *routing labels*, not evidence that a behavior
changed. This section is the honest dashboard — status + evidence/commit.

## Verified fixed (evidence)
- **Completion-instruction mismatch** — the self-harness answerer gets a
  dedicated `finalize @T` hole card, not the generic `resume expr`
  (`engine::answerer_hole_card`; core-tl2 `c477a63c`). Codex confirms.
- **Sync-blocking `OperatorGate`** — consistent across impl + driver bridge +
  plan (`61897716`). Codex confirms.
- **Fork/fanout cardinality + child cleanup on all exits** (`ecfaad4e`). Codex
  confirms fixed.
- **AskUser = explicit constructor arm**, not JSON-key probing; both outer +
  answerer form chains have bounded reprompt loops (`c477a63c`). Codex confirms.
- **`TurnStart.source` records extracted Haskell** — WS4 (`c477a63c`). Codex
  confirms the observability gap is closed (but see F15 below).
- **7-pane observatory replaced by the minimal gate** (`54a9196c`). Codex
  confirms the old-endpoint findings are obsolete.
- **fmt gate** — reconciled to the flake's stable rustfmt 1.8.0 (`this branch`).

## Dogfood-blocking — IN FLIGHT (dogfood-wiring dev)
- **F1** real web gate not connected to the real driver (two disconnected
  binaries). — wiring now.
- **F8** answerer include path disconnected from `--harness` (sibling
  `HarnessTypes` won't resolve under the real bin). — fixing now.
- **F5** production wires only `LogObserver`, no composite → no durable outer
  transcript. — folded into dogfood-wiring (add a fanout Observer + JsonlObserver).
- **F10** focused-panel SSE race: after a submit, the Form→Idle tick is DROPPED
  because focus is in the panel → operator sees a stale form, resubmits, gets
  "no form pending". Breaks the actual dogfood UX. — flagged to dogfood-wiring
  (revision-tag interactions; always apply cross-revision transitions); focused
  follow-up if it balloons.

## DESIGN DECISION for the human — F2 (fork capability contract)
Fork's reuse-approach re-exposed **bare `runLLMTurn`** on the answerer row
(`[AskUser, RunLLMTurn, Finalize]`), so "controlled bounded recursion" is now
PROMPT POLICY (`ANSWERER_FRAMING`) backed only by the coarse `LOOP_INFERENCE_CALL_CAP`,
not a structural limit. Also: "recursive fork" is mislabeled — first-level fork
works, but a fork child that itself forks / `askUser`s is hard-rejected (v1). The
options (this is exactly the [[type-system-expresses-capability]] axis):
  (a) accept prompt-policy fork + FIX the docs to say so honestly (cheap now);
  (b) give fork its OWN effect/constructor so the answerer gets fork-shaped
      delegation WITHOUT bare `runLLMTurn` (structural — the tighter boundary);
  (c) hide the raw verb at the generated Haskell surface.
I accepted (a)-for-v1 during recovery (reversible). Recommend: do (a)'s doc
honesty now (fix the "recursive"/"does not reopen" claims), hold (b) as the
principled follow-on. **Awaiting your call on (b).**

## Post-recovery ROBUSTNESS WAVE (parked, with Codex's acceptance criteria)
- **F3 [RESOLVED]** lifecycle: unconditional `Idle` is
  the cosmetic half — needs `Failed`/`Poisoned` or an error guard that
  restores/discards every mutable resident component before publishing `Idle`.
  Landed `4a8c9b95`: `SelfHarnessState::Failed{reason}`/`Poisoned{reason}`; an
  errored cycle discards the answerer, its framing, the cycle compaction, the
  inference counter and `self.outer` (which may be parked mid-fragment on a
  hole) before publishing `Failed`, so the next cycle re-bootstraps from source;
  a bootstrap failure *while recovering* escalates to `Poisoned`, which
  `run_one_cycle`/`run_loop`/`restore` refuse. Closed by mutation check
  (`650b85a7`), not by a green run: removing `self.outer = None` from the
  discard turns the recovery test RED with `session is suspended on
  continuation scont_1` — the parked-mid-fragment session the discard exists to
  drop; restoring the unconditional `Idle` turns it RED with `an errored cycle
  must publish Failed`. Both reverts left the tree byte-identical. Verifying
  also surfaced a gap the original landing missed: a FRESH driver's bootstrap
  failure left `lifecycle()` at the cosmetic `Idle` (the escalation only fired
  when recovering) — it now publishes `Failed`, covered by
  `fresh_driver_bootstrap_failure_is_failed_not_idle`. 4/4 green across
  `selfharness_lifecycle` + `selfharness_spine`.
- **F4 [RESOLVED]** state+compaction = ONE generation-tagged checkpoint.
  Acceptance: after a crash at every write boundary, restart selects a state +
  summary from the SAME committed generation + harness source. Resolved
  `df4ab614`: one `Checkpoint{generation, state, compaction, harness_source}`
  written atomically (`.tmp` + rename) at the end of `run_one_cycle`'s success
  path — so the acceptance path is durable too, not just `run_loop`. A mid-loop
  compaction updates memory only and never commits alone, so a crash mid-loop
  restores generation N's state *and* generation N's summary. `HarnessSource`
  gained a content fingerprint; a restore-time mismatch emits
  `Event::HarnessSourceChanged` rather than blocking (an edited harness file is
  the point of self-iteration). The `state.json`/`compaction.txt` pair and their
  helpers are deleted, not left as a fallback. `selfharness_persistence` covers
  the mixed-generation, same-generation, monotonicity and torn-file cases.
- **F6 [RESOLVED]** `flush_effects` drains then discards write failures +
  advances `effect_seq` — don't advance past a failed append; fail the turn or
  queue for retry. (Note: the scoped self-harness stacks emit NO `Event::Effect`
  by construction, so this bites general Agent nodes, not the dogfood path.)
  Resolved `e999a2b7`: returns `Result`; on the first append error it stops,
  restores the failed record and every record after it into the node's trace
  (ahead of anything concurrently pushed), leaves `effect_seq` at
  last-successful+1, and returns the error. All 7 call sites propagate — one
  mechanism, fail-the-turn, not a silent retry alongside it.
- **F7 [RESOLVED]** per-node turn race: a turn LEASE must cover snapshot →
  provider await → log append → resident run → outcome publish (broader than
  `SessionRegistry`). Resolved `e999a2b7`: RAII `TurnLease` on
  `NodeConvo::turn_lease`, acquired at exactly one layer per turn —
  `drive_turn`, `summarize_turn`, and each `answer_*`; `run_to_hole_or_done`
  and `follow_up` loop `drive_turn` without acquiring, and `drive_answerer_to_value`
  reaches `stream_turn` directly, so no call chain acquires twice on one node.
  `tests/turn_lease.rs` asserts one success + one `TurnInFlight` on concurrent
  turns, `turn_seq` advancing by exactly one, and lease release on the error
  path. Known gap: `eval_in_binding` is `pub`, has no callers or tests in the
  workspace, and takes no lease — routed to the quality sweep as a
  delete-or-lease call rather than leased speculatively.
- **Crash recovery [RESOLVED]** — the durability claim is now tested end to end
  through the production path (`87c1de8f`, `tidepool-web/tests/crash_recovery.rs`).
  Spawns the real `tidepool-selfharness` via `CARGO_BIN_EXE`, waits on a durable
  `TurnStart` marker until the process is genuinely mid-turn (that marker is
  logged before the block's GHC compile, so the kill lands inside a real
  compile window), SIGKILLs it by its own spawned PID, restarts the identical
  binary against the same `XDG_CACHE_HOME`, and asserts via
  `persistence::load_checkpoint(driver.checkpoint_path())` — no hard-coded
  filename or JSON shape — that `generation` went 1 → 3. Generation 2 alone
  would mean the restart began from `initialState`, so the assertion
  discriminates resumption from a fresh start. `.tmp` absence is checked
  immediately after the SIGKILL, covering the torn-write case at the crash
  boundary. A `Drop`-based `ChildGuard` reaps both children on every exit path.
  The two internal waits are a progress-stall watchdog (fail on no new durable
  event for 500s; 600s/800s absolute ceilings), not flat deadlines — a flat
  deadline flaked under contention that ran GHC 4-6x slower than baseline, and
  a stall window discriminates "slow under load" from "actually broken" in a
  way wall-clock cannot.
- **F9** unknown/malformed suspension constructor → a classification ERROR
  naming the constructor, not a silent `Ask` wildcard (still open for general
  callers + the malformed-`AskUserWith` case).
- **F11** `OperatorGate` returns `Submission`/`()` — make it a `Result` with
  cancelled/disconnected/EOF/malformed/superseded (the reprompt cap is a
  backstop, not error handling; a closed-stdin `await_continue` currently
  returns success → non-auto run loops forever).
- **F12** outbound `state_out` uses the structural renderer, not the author's
  `toJSON` — evaluate Haskell `toJSON` in the resident session, or narrow the
  contract + fix the misleading "ToJSON/FromJSON non-inverse" diagnostic.
- **F13** `HarnessEff` (`[RunLLMTurn]`) knowingly disagrees with the real outer
  row (`[RunLLMTurn, AskUser]`) — reconcile the named boundary or introduce
  explicit base/form-capable aliases; don't keep a stale alias as fiction.
- **F14** `retire_answerer` drops the session but writes no terminal/cancel
  event to the durable tree → live session and logged lifecycle diverge. One
  cleanup op should update both ownership and the tree with a reason.
- **F15** `TurnStart.source` carries the whole model reply on a prose-only
  (`NoBlock`) turn — two incompatible meanings. Use a separate `NoBlock` event
  or make `source` optional.
- **F16** `AppState::publish` silently replaces a pending interaction (drops its
  oneshot → old caller continues as if answered). Reject-while-pending or make
  supersession an explicit error.
- **F17** historical W1/W2/WS narration buries current invariants (esp. the
  `answerer_decls` comment). Move chronology to plan/ADR; keep comments to
  current invariants + failure behavior.
- **F18** GUI accepts structurally invalid forms (null int, omitted enum) and
  relies on distant Haskell re-prompt with no per-field error; `FormSpec` dup
  keys/tags unvalidated. Validate at the server boundary for usability (Haskell
  stays the type authority).

**Throughline (both reviews agree):** the risk is scar tissue — documented
exceptions ("stale-but-unused", "rides along", "dropped rather than aborting")
accumulating exactly where capability/durability guarantees should be strongest.
The consolidation (F3/F7/F14 — one transactional boundary per orchestration
step, the four state machines unified) is the wave's spine. See [[no-scar-tissue]].
