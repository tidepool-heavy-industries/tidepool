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

- fork-subsumes-split step 2: atomic subtree depth/total-node budgets,
  then raise MAX_FORK_DEPTH (the containment landed 2026-08-22 is parity,
  not the goal).
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
