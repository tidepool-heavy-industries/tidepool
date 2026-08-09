# Overnight plan — 2026-08-09 (Inanna in transit; root autonomous)

Designed with Inanna pre-flight. Root executes this decision tree on gates,
not guesses. Every spawn carries the standing rules verbatim (fold-cadence,
receipt doctrine incl. named guards + completed-vs-total + instrument
naming, parent-absolute broker + detach + one-brokered-leg, per-run cap 1,
inherited-red A/B). Nothing pushes to any remote EXCEPT
`harness-interaction-surface`, which root pushes at green points so no work
is lost.

## Standing constraint from Inanna: fold first, then load

Long-running lanes get a chance to MERGE before new scope. Concretely:
agent-wave and worktree-wave submit their green checkpoints when their
current items land; the next phase runs as FRESH lanes on the post-fold
tip. No queue extension onto live lanes.

## Codex model policy (Inanna, binding)

All overnight headless-worker runs use **gpt-5.4-mini** — cheap, dumb,
good enough to test plumbing. Don't waste it, but no spend anxiety at this
tier. NEVER gpt-5.6-terra (their sonnet/opus tier) overnight. Ephemeral
threads, small synthetic tasks, temp workspaces, stop-and-hold on anything
anomalous (auth prompts, config mutation, rate-limit walls).

## Chain A — PRD 14 completion + PRD 18 substrate (gate: dev-1 finishes)

1. generic-surface submits → root folds + runs the PRD 14 acceptance audit
   (criterion-by-criterion, not green-suite-as-proof).
2. Spawn **checkpoint-persistence** TL from the (revised) successor spec on
   their branch: fix the confirmed render.rs round-trip corruption paths in
   the EXISTING codec, move checkpoint persistence onto
   genericToJSON/genericParseJSON, pin the golden matrix, loudness-as-tests;
   Form fixture migration + implementation deletion ride along. No new
   codec, no Occurs port, no invented finiteness guard.
3. Push tip.

## Chain B — PRD 18 vertical core + PRD 19 coupled seam (gate: BOTH
## agent-wave and worktree-wave folded)

1. agent-wave: mode-encoding folds → submit checkpoint → root folds.
2. worktree-wave: L4 lands the RELOCATED (<|>) fix (author-facing imports,
   emits_helpers_for predicate, eval_prep hiding deleted after the
   compile-without gate) after boot-vocab's checkpoint fold → submit →
   root folds.
3. Spawn **agent-core** TL on the merged tip, single-lane over both crates:
   - The vertical core: spawn one real gpt-5.4-mini worker → generated
     tool call → parent M-effs handler → same child resumes →
     Generic-decoded terminal result → authoritative receipt into State.
   - The coupled-spawn seam: spawnAgent consumes/allocates its managed
     worktree per PRD 19's locked coupling (WorkerRun result shape,
     one worktree per agent, second binding fails explicitly, rebind only
     after terminal/release).
   - Registry/lifecycle per final PRD 18: tagged pokes
     (whenSafe/interrupting), AgentWentIdle as ordinary outcome,
     drainMailbox arrival scheduling, detached-but-running reattach by
     AgentReference, liveness/staleness observations out / policy in.
   - Opportunistic spike-checklist confirmation (park duration,
     steer-while-parked ordering) recorded as fixtures when answered.

## Chain C — realm step 4 (gate: extract-wave's boot-site fold notice)

Spawn **realm-step4** per standing material: registry-only ResidentSession
conversion (verdict §3 blast radius: repl server.rs:516, harness.rs
run_child/ChildSuspended arms, resident_session tests), step 6 (realm
display names), the envelope-tag agreement check (ledger item 3
resolution), review item 11's structural completion. Prereq also includes
resident-pending-fix's clear-after-consume fix being folded (same file,
resume path) — fold order: their fix first.

## Chain D — dogfood re-entry (gate: Chain A step 1 complete + review pass)

1. Root review pass over the merged tree.
2. Spawn **form-api** dev (AFTER dogfood-observability folds — shared
   tidepool-web territory): a testing-convenience HTTP surface on the
   operator gate — GET current pending form (JSON, the derived FormShape
   wire) + POST submit answer (FormAnswer wire) — so root can drive real
   form interactions without a browser. Plain JSON, no auth changes, test
   convenience explicitly, documented as such.
3. `scripts/redeploy.sh` + reconnect (the deferred wire-break redeploy).
4. Bounded dogfood smoke: wizard acceptance-style run driven end-to-end
   via the form API; the owed TIDEPOOL_GC_POISON bounded run for the
   garbage con_tag intermittent. Results in a morning report, not acted on
   beyond obvious small fixes.

## Continuing regardless

extract-wave (item 0 waves 2-3, D2 with mandatory-roots sweep + empirical
attribution first, E6), root devs finishing (mock-derive commit 1 =
box-wide red clear; archaeology-sweep; strict-classify;
resident-pending-fix; dogfood-observability), harness respawn wave two
(hole-card synopsis + error-coordinates) after dogfood-observability
folds.

## Explicitly held overnight

- resident.rs pending/ChildSuspended: untouched until Chain C's gate fires.
- No remote pushes except harness-interaction-surface.
- No gpt-5.6-terra usage.
- Dogfood findings beyond small obvious fixes: recorded, not acted on.

## Late additions (Inanna, pre-boarding)

- TOKEN ECONOMY: weekly limits near. No speculative work; idle-time code
  tracing BELAYED (codex does it later); reports lean, receipts complete;
  message overhead minimal.
- env-skip-fail dev live: missing-env test skips convert to loud failures
  (ruling: the stated fail-loud convention was already the law; skip-as-pass
  sites nonconforming). Duration-vs-work-claimed adopted as doctrine.

## Amendments after ChatGPT plan review (pre-boarding, Inanna-endorsed)

- CHAIN B SPLIT (Inanna: "agent-core is doing a lot" — agreed): overnight
  spawns ONLY lane 1 of five: (1) one-cycle clean-spawn vertical — one
  worker, one cycle, no cross-cycle, no reattach, provisional/internal API
  shapes allowed. Lanes (2) coupled-spawn failure/saga matrix, (3) durable
  poke/interrupt ordering, (4) cross-cycle detach/reattach + tool-handler
  wakeup, (5) mailbox/staleness seam: DEFERRED, each gated on design
  freezes the review demands (typed SpawnError/PokeError/AttachError,
  agentReference op, the reattach-vs-wake-vs-forbid decision for
  cross-cycle tool calls, the coupled-spawn state machine). Those are
  MORNING PRD work, not overnight implementation.
- MODEL POLICY: runtime-resolved cheap-plumbing tier — query model/list,
  prefer gpt-5.4-mini if present else gpt-5.6-luna (Inanna: cheapest;
  luna fine if ~same price); record the EXACT resolved model in every
  receipt. No hardcoded slug.
- CHAIN C SCOPE ADD: parking contract/code reconciliation folds into
  realm-step4 — privatize ContinuationId/RealmId (minting authority),
  crate-private ContinuationFrame, typed lookup errors (not stringly
  Handler), bind resume to a runtime-owned handler-row witness. Until
  then the contract file gets an UNVERIFIED-vs-code banner (morning).
- CHAIN D ENDPOINT HARDENING: form-api is config-gated OFF by default,
  loopback-only, per-prompt nonce, explicitly test-only — an operator
  answer is authority regardless of the word "testing".
- Ledger state-table (state/fixed-by/verified-on/next) + PRD 14 status
  truing: MORNING items.
- NO LIVE-MODEL TURNS IN TESTS/AUTOMATED CODE (Inanna): committed test
  suites use replay/mock providers only (ReplayProvider exists for this).
  Live-model legs — dogfood smoke, agent-core lane 1's real-worker
  demonstration — are DELIBERATE, manually-triggered runs by root/a lane
  with receipts, never wired into suites, battery tiers, or anything that
  runs on invocation. Morning: fold this into CLAUDE.md/testing docs.
