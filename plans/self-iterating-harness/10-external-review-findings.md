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
