# Benchmark track: the citeable capability line

Goal (Inanna, 2026-08-09): a harness performance metric that is (1) about
capability, not substrate, and (2) reasonably citeable — one line going up
across harness generations. Substrate/latency curves are explicitly NOT
the goal (useful internally; not the pitch).

## Revised (same day): three tiers by marginal cost — no agent-farm budget;
## real tasks are the substrate, not a separate benchmark workload

**Tier 0 — telemetry from real use (FREE, starts at dogfood launch).**
Metrics that normalize across heterogeneous tasks because they measure the
HARNESS: first-compile success rate (the dialect thesis as a number),
corrective-retry loops per hole, turns-to-finalize, tokens per completed
hole, operator interventions per session. All already in the event log —
the "benchmark" is a fold over jsonl that exists. The citeable artifact is
a longitudinal case study ("first-compile success X%→Y% over N generations,
M real turns"), which serves the pitch better than a leaderboard.

**Tier 1 — distill real tasks into a replayable corpus (near-zero).** The
repo's own fixtures-from-real-defects philosophy, one level up: after a
real session, distill it into a replayable mini-task (scripted operator
via replay machinery, pinned snapshot, typed success predicate on the
Finalize value). Suite grows organically, stays 10-20 tasks, one
slot-batch per generation. Pass-rate + efficiency per generation is the
line, on tasks real by construction.

**Tier 2 — public benchmark, sparse + small (bounded, per-release
ritual).** A 10-20 task Terminal-Bench subset once per GENERATION (weeks
apart): tens of dollars per point, like tagging a release. Keeps the
external-comparability anchor. EXPLICITLY OUT: continuous runs, full
suites, standing compute.

Original tier-2 framing (retained for when budget allows):

**Terminal-Bench first.** Public, recognized, and the thesis's home turf —
"bash++ for LLM agents" is tested exactly where agents do terminal work.
Graph: fixed model (+ pinned temperature), two lines per generation:
  (a) plain bash-tools baseline agent
  (b) tidepool harness, generation N
The citeable claim is SAME MODEL, GROWING GAP — attributing improvement to
the harness rather than the model. SWE-bench Lite (standard subset) is the
second line once the first exists: max recognition, higher standup cost.

## Discipline that makes it citeable (and Goodhart-guards it)

- Iterate on dev splits + the in-house task suite; REPORT held-out only.
- k runs per point, error bars; pinned model/temp/task-set versions;
  transcripts published per point.
- Substrate metrics (latency stage table) tracked separately; neither
  curve may regress when the other improves.
- Generation = a tagged release; results JSONL committed per tag so the
  graph accretes in-repo.

## What the harness needs first (small)

- Agent stack composed with fs/exec/git effects (decl-list scoping — the
  effects exist; a coding/terminal stack is a row definition away).
- Non-interactive driver (exists: replay/selfharness machinery).
- A runner harness — itself written AS a tidepool harness (forkAll over
  tasks, graders finalizing typed scores): the graph is produced by the
  thing it measures.

## Relation to the subsumption thought

Long-range direction (same conversation): the self-iterating harness may
subsume exo — orchestration decomposed into SMALL TYPED EFFECTS on the Git
precedent (simplified worktree/branch effect, session-spawn effect,
messaging effect, liveness), NOT one monolithic wrapper. Role-scoped
stacks via decl-list scoping give capability-by-construction (a dev's row
has no spawn effect). Everything this campaign converted from prose to
mechanism (receipts, READY, allowlists, choreography) becomes a type:
receipts are `Finalize Receipt` crossing in-heap. First step when its wave
comes: the brain moves into the harness (state, receipt folding,
choreography) while exo remains the process shim.

Sequencing: dogfood proves the loop → benchmark wave (Terminal-Bench +
baseline + runner) → generation 1's first dot → every later wave has a
scoreboard it must not regress.
