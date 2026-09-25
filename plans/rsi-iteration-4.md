# RSI iteration 4 — inter-wave implementation

Approved after wave 11. Product source `ec012f0`, final interview/handoff
`3bf45e2` in exomonad-harness. The next harness product wave is separate;
no Exomonad adapter implementation or credentialed provider run is included.

## Parallel ownership

- Request-scoped review: `review_contract_options`, preserved worktree
  `.claude/worktrees/request-scoped-review`; runtime type/binding proof,
  actor request access, then authored acceptance tool. Owns request wiring
  in resident_actor; coordinate helper reload edits before touching it.
- Session helpers: `wave8_timeline`; existing workspace overlay/source owners,
  helper mount, publication, fork snapshots and reload. Coordinate actor seam
  with review lane. Helpers are session-owned, outside product Git.
- Coordination: `review_docs`; workspace typed continuation, Routing/Observe,
  findings result, compact delivery and compiled fixtures. Sol integrates;
  actor routes at most two repair rounds before escalation.
- Supervisor: evidence runner, compiler wrapper repair, request-update
  observation, seeds, integration/pins and final rehearsal. Queue independent
  lanes as slots free; avoid overlapping edits and concurrent broad builds.

## Accepted contracts

Expose `.exomonad/helpers/SessionHelpers.hs` and `SessionHelpers.*`, with new
modules allowed during a run. `reload_helpers` atomically checks/publishes
through existing source ownership independently of AgentSpec. Fork both the
editable draft and last valid revision; invalid drafts do not become active.
Each actor owns its publication pointer. Deletion cannot reveal lower copies.
Existing closures remain stable; subsequent compilation uses the new revision.

Review derives basis/candidate from the active typed request and checks exact
binding identity, canonical type evidence and checkout source. Retain existing
Replies settlement. No generated-Haskell submission or second request registry.

Test runner selects/builds one artifact, freezes it where needed, lists/runs it
with expected counts and retains source, command, artifact and output evidence.
Jev interprets evidence; deterministic code owns execution facts. Seed helpers
are deliberately remixable and inherited, not mandatory policy. Small manual
live Jev experiments are authorized; no large unattended runs.

Request-update observation extends existing request/watch owners, distinguishes
presentation from incorporation and never retries uncertain delivery blindly.
Repair generated wrapper type references using compiler-resolved information,
not a module whitelist or silent removal of necessary constraints.

## Integration bar

Focused tests for helper rollback/new modules/deletion/fork isolation and no Git
mutation; review stale/revised/no-active/wrong-type/shadowed input; typed
accept/repair/findings/convergence; test-runner counts/failures/artifact identity;
update observation races and cleanup; compiler wrapper regression fixtures.
Compile affected downstream targets, format and diff-check. Pin/mirror shared
workspace and bump prompt fingerprints with verified surfaces. Preserve
`plans/harness-adoption.md` without edits.

Final bounded rehearsal: a parent customizes a Bash/Jev test helper, parallel
Luna children inherit it, typed actors route review/repair, Sol integrates.
Measure overlap, Luna work, independent-review value, tool calls/model rounds,
helper reuse and avoidable relay. No next product-wave launch is implied.
