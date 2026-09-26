# Post-wave17: reliable helper composition and lower coordination cost

## Status and evidence

Planned 2026-09-26; implementation has not started. Three independent Sol Medium
planning agents traced source ownership; the supervisor reviewed their proposals
against the wave17 handoff/interviews and root transcript. Agents performed no
builds or edits. The next step is an isolated worktree implementation batch.

Run: `ae20a047-cb39-41e1-8621-efd3035fd719`; branch `rsi/wave17` in
`/home/inanna/dev/exomonad-harness-runs/wave17`. Checked source `6c28a79`,
stop handoff `16147ef`, release follow-up `837040f`. Read its
`docs/wave17-handoff.md`, `docs/interviews.md`, `docs/exomonad-friction.md` and
`docs/automation-trials.json`. Tidepool baseline `6a40f7e6b`; shared workspace
`2599a643`; harness supervisor master includes launch record `c0c7846`.
Resolve full OIDs and current dirty state before creating worktrees.

Observed gains: root used runCheck and runBrowserCheck on real acceptance.
The native ReviewFlow reviewer launched and returned a typed result. A bounded
root transcript count found 50 bash, 49 haskell, 19 status, 4 lookup,
14 apply_patch, one reload_helpers and one read_output call, with zero
write_stdin. These are tool calls, not model rounds or measured savings.

Important corrections to the first interpretation:

- Source points to root seed shadowing, not prior-run draft reuse: root mounts
  a run-scoped generic helper draft over tracked project helpers. The seed
  differs in exactly the README/export/deleted BrowserChecks paths seen at
  startup. Reproduce this before claiming a verified runtime fix.
- Root was correctly refused cleanup of the review coordinator's fork group.
  The coordinator must exercise its own existing authority.
- Handoff artifact paths are actor mount paths. Their absence at the equivalent
  host checkout path does not prove lost evidence.
- check/init/host compilation has differing path/layout inputs; equivalent
  repeated work is not yet established. No cache or validation bypass approved.
- Stash was already prohibited. It was not newly prohibited after root used it;
  the launch briefing failed to carry the standing constraint clearly enough.

## Design defaults

1. A project helper directory initializes the root draft when present. Use the
   generic seed only when that directory is absent; do not merge seed files into
   an authored directory and silently resurrect deleted modules. Preserve the
   user's draft and explicit publication semantics. Children inherit snapshots.
2. Explicit ReviewFlow cleanup after interviews, requested through the coordinator;
   no automatic retirement on a verdict, and no broadening root authority.
3. Haskell composes workflows; existing Rust owners retain jobs, resources,
   artifacts and authority. No new scheduler, job registry or durable log.
4. Recover/observe never silently reruns a command. Source identity, execution,
   test acceptance and cleanup remain distinct.
5. Use existing standalone release scripts and deterministic provider. No adapter,
   credentialed inference, live demo/Tailscale changes or new hook semantics.
6. Keep helpers available for at least three exposed waves. Improve ergonomics
   from actual opportunities; do not add a helper-use quota.

## Implementation parcels

### A. Root workspace integrity — first foundation

Owner: one Sol Medium lane for the two interacting mount defects.
Paths: `bridge/facade/src/actor_host/workspace.rs`, `workspace_tests.rs`, with
`exomonad/worktree/src/{create,git}.rs` helpers only where necessary.

- Initialize root helper draft from project helpers, falling back to the seed
  only when absent. Coordinate with B to keep one initializer rather than two.
- Fix relative submodule Git metadata under the stable actor mount. Reuse the
  existing GitCli resolver and child-worktree normalization knowledge. Prefer
  a generated absolute gitdir file overlay; do not mutate host `.git` files.
  Keep ProcessMountBoundary generic; its depth ordering already supports this.
- Tests: clean tracked custom helpers remain visible and clean inside mount;
  absent helpers get the seed; host bytes unchanged; differing/deleted custom
  modules preserved; real relative/nested submodule metadata works from stable
  path; broken metadata refuses before model launch.

Payoff: remove startup investigation and false dirty-source results before any
model acts. This is not a prompt-only repair.

### B. Helper publication and inheritance contract

Owner: separate Sol Medium lane after A's initializer contract is agreed.
Paths: `bridge/facade/src/exomonad/source.rs`, source-layer tests and focused
`actor_host/source_reload_tests.rs`; receipt rendering only if necessary.

- Root initialization, mutable draft, active revision and child snapshot must
  have one authoritative lifecycle. Remove duplicated initialization policy.
- Report the published revision and rejected/unpublished draft distinction;
  prove same-actor import and child inherited import work on the active source.
- Keep invalid drafts intact; last valid publication continues to work. Parent
  republish does not silently mutate an existing child. Do not auto-publish edits.
- Test clean project override, invalid draft, explicit reload, fork, and later
  parent reload. Do not implement a source cache to solve a visibility problem.

Startup eager publication, if needed, must use the existing validation entrypoint;
first establish which publication boundary the current API promises.

### C. Recoverable check gates

Owner: shared-workspace Haskell lane.
Paths: `Project/FocusedGateExample.hs`, `Project/CheckResults.hs`, their checks
and session seed. Extend `Project/TestEvidence.hs` only as the evidence owner.

- Make retaining and reopening an original focused run easy across notebook
  cells. Reuse the existing watcher state, FocusedRun and command identity.
- Supply one compiled start/bind/read example and a recovery entrypoint from
  retained original-job evidence. A name alone is not authority or a lookup key
  into a new registry. Missing bindings must not imply missing jobs.
- Preserve failed/refused/expired/incomplete states. Recovery submits zero new
  commands and does not manufacture a successful preparation receipt.
- Tests: scope lost after start, late completion, original job recovery, expired
  output, wrong source, dirty tree, zero selection, cancelled/unclean completion.

Payoff: eliminate the browser worker's recovery/reassignment round.

### D. Review coordinator owns cleanup

Owner: shared-workspace Haskell lane, disjoint from C.
Paths: `Project/ReviewFlow.hs`, `Project/ReviewFlowChecks.hs`, checked example.

- Add an explicit coordinator operation that derives its own reviewer groups
  from retained requests and invokes existing planCleanup/executeCleanup.
- Retain actual cleanup receipts in final flow state. Caller can obtain the
  terminal result, interview reviewers, then request cleanup before R.finish.
- Guard pending review/correction/repair; preserve stale/refused/retaining states.
  Repeated observation must not cancel unrelated work or assert released resources.
- Tests cover no reviewer, successful review, repair history, pending work,
  interview-before-cleanup ordering, cancellation and incomplete release.

No new runtime cleanup primitive unless a narrow reproducer proves a missing one.

### E. Executable standalone release workflow

Owner: harness lane, based on reviewed wave17 source; no Tidepool engine edits.
Paths: existing `scripts/prepare-browser-harness`, `scripts/launch-browser-harness`,
focused script tests and release docs. Reuse production browser consumer checks.

- Make the documented first-use path invoke the production scripts, with bounded
  validation of missing parent, existing path permissions, assets and binary.
- Fail explicitly for unsafe existing paths; do not silently chmod unrelated data.
- Exercise staged binary from unrelated cwd with isolated DB/port, readiness,
  authenticated deterministic commands, reconnect/reopen and exact-process cleanup.
  Reuse existing HTTP/WebSocket test utilities rather than a second client stack.
- Retain source/binary identity and cleanup. Missing/stale assets, occupied port
  and failed startup must not leave a process behind or become a product pass.
- Docs point to the tested entrypoint. No manually copied shell sequence drifting
  independently of its implementation.

Payoff: code catches the missing-parent/umask class before three prose reviews.
This is a reusable narrow release gate, not a generic deployment framework.

### F. Operator evidence provenance

Owner: Rust facade lane.
Paths: `actor_host/overlay_resource.rs`, existing layout metadata and
`bridge/facade/src/run_map/{mod,metadata}.rs` or operator surface as appropriate.

- Expose read-only provenance from the existing build-overlay owner: run/actor,
  logical mount path and actual retained layer(s). Integrate with run-map.
- Do not assume the file is in upper: inherited/published layers and retirement
  need explicit resolution or unavailable/unknown results.
- Prefer bounded compact receipts for routine verification; do not copy full logs
  into prompts or create a second artifact registry.
- Tests: current upper, inherited lower, missing/retired resource, traversal
  refusal, incomplete mapping; operator cannot acquire new write authority.

Payoff: operator verification should not require filesystem archaeology.

### G. Prompt and assignment discipline

Owner: prompt lane after A–D API details; early prose outline can run independently.
Paths: harness role prompts and orchestration guide; core catalog only if shared
instructions change, with one integration-owned version/fingerprint bump.

- Carry standing no-stash/reset/path-checkout, dirty-file preservation and
  pathspec-commit rules in the actual launched instructions, not just supervisor
  history. Preserve the known stash object; do not apply/drop it during this batch.
- Publish intended helpers before delegation; name API and source in tasks.
  A failed later notification does not establish incorporation.
- Assign a narrow manifest/lockfile amendment with a repair when required, or
  name its owner before dispatch. Avoid serial blocked→amend→rebase surprises.
- For runnable operator docs, review first-use preconditions and executed evidence;
  source-only review can remain legitimate but cannot claim execution.
- Keep examples compiled and avoid making evidence-only tasks recurse into review.

### H. Small Haskell check-plan experiment

Owner: C lane follow-on or another lane after C lands; same files must not diverge.

Build one project-specific composition for the four Engine/Store checks and
browser gate actually used in wave17. One supplied candidate, named checks,
explicit per-command memory, compact terminal summary and retained per-check
receipts. Use existing watchChecks aggregation and Rust command admission;
handle partial start refusals honestly. No generic workflow engine or Haskell
resource scheduler. Preparation is only attached to checks needing it.

The root already batched two runCheck starts per notebook call. The experiment
must improve on that baseline: fewer bindings, evidence reads and duplicate
notices, not merely a shorter spelling. Keep individual checks reusable.
Failure tests include one refused start, one failed check, dirty source and
mixed terminality. Gate does not claim all passed when a member never ran.

## Investigations with bounded implementation follow-through

### I. Launch/compiler cost

Owner: performance lane. Existing compiler instrumentation/cache is the owner.
Trace check → init validation → root bootstrap with exact source/toolchain,
generated driver/imports, ordered include roots and cache hit/miss reasons.
Wave17 root bootstrap compiler response was 44.7 s; equivalence of earlier
compiles is unproven. First deliver a phase/key comparison. If identical work
is demonstrably repeated, reuse the owning immutable result with invalidation
and failure tests. Do not bypass validation or add another cache. A path-identity
redesign requires supervisor design review before implementation.

### J. Resource interruption and concurrency

Owner: resource investigation lane, no automatic tuning in advance.
Find the source-audit exit137 original job; correlate reservation, process status,
cgroup memory events and competing jobs. Distinguish kill, OOM, cancellation and
unknown. Propose a focused owner fix if proven; otherwise improve the relevant
receipt and scoped task memory guidance. No universal low-worker default based
only on an exit code and no restart of shared daemons.

### K. Reusable RSI turn-graph audit

Owner: audit tooling lane, extending existing run-map/trace projections.
First deliver an evidence-only wave17 report; implementation must avoid conflict
with F's metadata additions (separate trace/review files or stage after F).

Join actor/provider/call/job identities using existing traces. Report start→wait/
read→decision sequences, helper setup/recovery, failed cells, repeated status,
review/repair/integration edges and slow compile/resource phases. Distinguish
calls, model rounds, latency and tokens; unknown links stay unknown. Bounded
optional Jev classification can flag ambiguous low-value follow-ups, with raw
provenance and an abstention path. It must not decide test acceptance.

Measure root and workers separately. Full contexts remain private and bounded.
Report plausible replaceable frontiers separately from observed round counts;
no causal savings claim from before/after counts alone. Use reports to choose
one next automation target, not build a concepts taxonomy.

## Worktree schedule and integration ownership

Supervisor owns shared decisions, API review, cross-repo pins, template sync,
one prompt fingerprint bump, final matched build and next-wave brief. Use Sol
Medium execution lanes, with fresh Astra consultation for consequential design
uncertainty. At most three child lanes active here; queue the rest.

1. Read exact wave17 reviews and preserve dirty helper/stash evidence. Establish
   harness integration base containing its checked product and our latest prompt
   baseline without discarding either history. No silent master overwrite.
2. First frontier A (mount integrity), C (check recovery), D (review cleanup).
3. Then B after A's initialization seam, E in harness, F in facade metadata.
4. H after C; G after settled API/examples. I, J and K investigations can replace
   a waiting lane; performance mutations follow only from measured findings.
5. Common-file changes are rebased in order. A and B share source/mount setup;
   F and K share run-map ownership; C and H share check APIs. No parallel edits
   to the same policy or independent competing implementations.

Every lane gets exact base OID, owned paths, consumers, failure checks and evidence
contract. Isolated worktrees; no broad batteries. Coordinate one active expensive
compiler/test worker at a time across lanes. Pure source review and lightweight
checks can continue in parallel. Never restart a shared daemon.

Each delivery: commit by explicit paths, changed targets compiled, exact focused
checks reported, diff reviewed for duplicate owners and magic-string control flow,
formatting/diff-check clean. Integration rechecks changed joins, not every unrelated
suite. Shared workspace commit must be published before pinning; sync template and
harness, verify exact final gitlinks. Keep dirty worktrees and all user commits.

## Next-wave gates and scorecard

Before launch: clean mounted root with authored helpers, validated helper import/
child snapshot, recoverable original check result, coordinator cleanup tests,
matched build and published pins. One native bounded ReviewFlow check must show
first turn, typed result and explicit release evidence before broad use.

The next bounded experiment is a whole acceptance plan with one terminal summary,
including the existing browser preparation. Record opportunity, actual use,
setup/refusal/recovery rounds, completion notices, missing evidence and frontier
calls. Keep the individual helpers for three exposed waves. Do not launch merely
because lanes finish; choose product work from the standalone milestone and the
new executable-release evidence. Interview before retirement.
