# Flight dogfood campaign (autonomous, ~7h offline operator)

**Status: planned 2026-08-22, runs during the operator's Denpasar→Hong Kong
flight.** Root (Claude Code session) drives everything; analysis via native
subagents (Agent tool — nothing to merge, no exo lanes). Operator reads
round reports on landing.

## Gate before the first round

1. Terminology stages complete enough that no model-visible text or data
   carries banned vocabulary (done as of f119ca98 for the companion; the
   identifier stage is dev-facing and does NOT block).
2. All in-flight lanes folded; full harness battery walked green.
3. `scripts/redeploy.sh` + rebuild `tidepool-selfharness` + kill old
   companion (by exact PID after `pgrep -a` inspection — never
   pattern-kill) + FRESH checkpoint (serialized-field rename requires it;
   archive old state to scratchpad first).

## Round mechanics (fresh state each round)

1. Stop companion; archive `$XDG`/tidepool/selfharness state to
   `scratchpad/flight-rounds/<n>/pre/`; delete checkpoint + transcript.
2. Boot fresh (nohup, `RUST_LOG=info`, dogfood XDG dir, numbered log).
3. **Robot operator**: a poll loop against the web form API — answers the
   seed-question ask with the round's scenario question, answers
   between-turn continue asks (up to the round's turn budget, then stop),
   answers any mid-run steering/approval ask with a canned
   "proceed on your best judgment" (per-scenario overrides allowed).
   This is the enabling piece: every operator touchpoint is an ordinary
   form, so a script answers them all.
4. After the turn budget: stop the companion, harvest logs
   (`log-*.jsonl`, journal, transcript, checkpoint) into the round dir.
5. Spawn ONE analysis subagent per round (fresh context): capability
   usage (forked? batched multiple waves? typed helpers defined and
   reused? delegated? asked?), frictions (correctives, compile errors,
   refusals, starvation, budget hits), compile-latency lines
   (`compile summary` — the memo-hit story is now measurable), verbatim
   quotes of anything confused-sounding. Output: one round report MD.
6. Aggregate across rounds at the end: recurring frictions ranked, with
   the prompt/mechanism fix each suggests.

## Compile-failure report cadence

Step 5's per-round analysis subagent reads compile errors by eye. The
`tidepool-compile-report` binary (`tidepool::compile_report`) folds the same
durable evidence — a harness's `transcript.jsonl` `AnswererRound.error` text,
plus this project's own eval-surface `eval-failures.jsonl`
(`tidepool_runtime::paths::eval_failure_log_path`) — into a ranked,
counted table instead: which unsupported construct (variable/type-
constructor/module scope miss, missing instance, JIT gap, import-grammar
rejection, wrapper-attributed failure) is reached for most, by which named
identifiers, and how first-try-compile rate is trending run over run. This is
the instrument root `CLAUDE.md`'s "the interface evolves as an optimization
loop" line asks for: surface changes driven by counted desire paths, not by
whichever error a reader happened to notice.

**Run it:**
```bash
tidepool-compile-report ~/.cache/tidepool/selfharness/transcript.jsonl \
                         ~/.cache/tidepool/eval-failures.jsonl
# or fold a whole flight's rounds at once:
tidepool-compile-report 'scratchpad/flight-rounds/*/transcript.jsonl'
tidepool-compile-report --format json ... > report.json   # machine-readable
```

**When:** at step 6 (aggregate across rounds), before writing the recurring-
frictions summary — the ranked bucket counts are the aggregate step's raw
material, not a replacement for it (the report ranks WHAT broke; the human
aggregate still judges WHY and what to do about it). Also worth a standalone
run after any single long dogfood/companion session, and periodically against
the live `~/.cache/tidepool/selfharness/` + eval-failures log outside a
flight campaign, to catch drift between campaigns.

**Ranked entry → pave-or-dam decision:** a bucket's top identifiers are
candidate constructs to either PAVE (add real support — a missing stdlib
function, a new effect, a JIT primop) or DAM (the model is reaching for
something that should not exist here — tighten the prompt/docs to steer
away from it instead). Which one depends on the identifier, not the bucket:
`variable-not-in-scope` naming a real stdlib gap (a function that should
exist) is a pave; the same bucket naming a hallucinated verb from a
different codebase's vocabulary is a dam. A bucket with a persistently
non-zero count and the SAME top identifier across multiple runs is the
strong signal — a one-off in a single round is noise, a repeat across
several independent runs is a desire path. `other` growing without a clear
identifier pattern is itself a finding: it means the classifier's bucket set
no longer covers what's actually failing, and the classifier (not just the
prompt) may need a new bucket.

## Scenario battery (one per round, fresh session each)

| # | Seed question targets | Capabilities exercised |
|---|---|---|
| 1 | "Compare three approaches to <X>; stress-test the winner" | fork batches, wave-then-wave, fold-in-scope |
| 2 | "Design a typed data model for <Y>; validate it on examples" | decl persistence, define rounds, session library |
| 3 | "Audit <small area of this repo>; propose and delegate a fix" | delegate (codex child), evidence gathering |
| 4 | "Investigate A and B concurrently, then reconcile" | async (fork), green threads, waitBoth |
| 5 | An underdetermined tradeoff question | askUser/steering (robot-answered), judgment recording |
| 6 | A question inviting wide fan-out | budget refusals handled gracefully, depth containment |

Pick X/Y/areas fresh per run from the repo itself so answers are checkable.

## Ambitious-scope queue for the flight (root's own work, between rounds)

- Atomic subtree depth/total-node fork budgets, then raise MAX_FORK_DEPTH
  (the containment landed 2026-08-22 is parity, not the goal).
- dup-e #2: one green dispatcher with a delivery adapter (after the
  naming map lands).
- Terminology stage 3 (freed regions) + stage 4 identifier renames.
- Step 4 PROPOSAL doc (companion collapse — design only, operator
  decides on landing; never executed unattended).
- Full battery walks at each wave boundary.

## Hard rules while unattended

- No new exo lanes without a queued decision from the operator; native
  subagents freely.
- Nothing outward-facing; no pushes; commits stay on the branch.
- A wedged companion round is stopped and logged, never force-fixed by
  editing state; the round report records the wedge as a finding.
