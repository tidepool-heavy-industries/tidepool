# Campaign supervisor: execute, review and consolidate

Your task is to implement B and qualify G2, with C investigation kept parallel.
You own technical decisions within [CONTRACTS](CONTRACTS.md), not just routing.
Read critical diffs and actual consumers. A pile of completed leaves, a clean
tree or many component passes is not delivery.

## First actions

1. Read the checkout's current AGENTS/build guide, this pack and the evidence
   index. Verify branch, HEAD, WIP, jobs and remote URLs. If another owner has
   moved the branch, inspect and incorporate that delta; do not reset it.
2. Record the actual documentation tip containing this pack. Preserve D's WIP.
   Start W, N, Q, H, C and resource investigation immediately in separate source
   checkouts. P1/P2/P3 can trace and prepare controls while P freezes the contract.
3. Merge `402528b8322615ab635adb8fca1cfc719a684523` into the campaign branch as a
   distinct reviewed prerequisite. Inspect its defining-Name/re-export behavior
   and conflict resolution; it is based before the logging reapply. Do not hold
   this tiny prerequisite behind the entire package migration. Q4 supplies the
   corresponding Rust deletion and focused controls.
4. Give P the concrete new base. P fills the wire/identity decisions in CONTRACTS
   and gets S/P3 review before incompatible producer/consumer edits. A bounded
   probe resolving a concrete contract uncertainty is useful; another broad
   architectural survey is not needed.
5. Give V ownership of exact gate commands and resource footprints immediately.
   Preserve a warm build checkout where safe. Do not wait for all code to finish
   before discovering missing test inputs or stale selectors.

## Assignments and events

Every leaf gets a written assignment containing role, parent, exact source OID,
isolated checkout/branch, owned paths or hunks, dependency contract version,
primary goal, two or three adjacent review questions, acceptance and escalation.
Require an acknowledgement with its observed base and first concrete action.
"Idle", "waiting for task", a dispatch RPC return, or an unconsumed task file
is not evidence of a running worker. Correct failed dispatches explicitly.

Children report proposal, contract-question, first-counterexample, review-ready
and settled events to their parent. Include commit and evidence paths, not only
chat prose. Use the actual harness's parent notification/wake mechanism: some
send-message operations do not start an idle turn. Verify how continuation works
once. Use a supported event wait when nothing is ready; do not make every node
poll every minute or invent a native Codex goal loop.

Parents continue the unblocked frontier, review promptly and integrate it. If a
child stops at capacity, preserve its branch and handoff, then wake or replace it.
Do not reinterpret stopped workers as successful or spawn a new speculative
implementation for the same unresolved contract.

Keep [STATE](STATE.md) current. The assignment artifact and Git are recovery
sources when volatile context or messaging is lost.

## Branches and shared source

Use one source writer per checkout. Reuse safe isolated warm checkouts or create
real Git checkouts under `/srv/swarm/checkouts/`; provision Buck storage before
building. Record base/tip and owned paths. Avoid creating a cold output tree for
every reviewer: read-only review needs no separate build products.

P owns a consolidation branch for P1/P2/P3. Merge its reviewed producer/consumer
cutover as one coherent tranche; do not put a half-version change on campaign.
W and N each return independently reviewable candidates. Q and H merge as soon
as their exact dependencies and review are satisfied. C stays separate unless
explicitly admitted before the qualification cutoff.

Shared-file hazards to coordinate:

- `certified_products.rs`: P's exact-link decoder, W's interface witness policy,
  Q4's Original membership refusal. Use named function/hunk ownership; merge
  independent sections in sequence and review the joined call chain.
- `CertifiedProducts.hs`: P output/link emission versus W's optional producer
  carry-forward. W's default custody-owner repair avoids independently redesigning
  P's output format.
- `GhcPipeline.hs`: structural prerequisite, Q3 diagnostic aggregation, D trace
  instrumentation and optional C demand changes. Preserve protocol events and
  don't resurrect the facade fallback while resolving conflicts.
- `LoggingTest.hs`: Q1 owns both logger checks and partial-acquisition cleanup.
- New Haskell fixtures: P3 coordinates ownership with P1. V registers/discovers
  existing targets; it does not change a test's semantic expectation unaudited.

Feature branches may be rebased with their sole owner before handoff. Never
rewrite published shared integration history or force-push away another owner.
Commit checkpoints and push the explicit `github` remote; verify `ls-remote`.
Local `origin` publication is not remote delivery. Do not leave accepted parcels
or useful C evidence orphaned after the campaign.

## Review gates

At proposal review ask: which owner knows this fact first, who actually needs
it, and can this step disappear? Are selection, storage and progress still
distinct? Is a broad inventory compensating for information lost earlier?
Could this type still be constructed in the invalid combination being repaired?

At candidate review inspect the exact base-to-tip diff, production constructors,
callers, error/drop paths, recovery and tests. Check removal of obsolete APIs and
deletion of their callers. A private enum plus a public unchecked constructor
does not enforce the boundary. No rendered error text should select control flow.

P3 reviews P1/P2; R reviews P's joined authority path and P3's oracles. R reviews
W/N. Reuse finished Q/C workers for independent Q/H review. Give corrections to
the context-loaded owner; after substantial repair require a fresh exact-tip
review. Review isn't a substitute for compilation or executed controls.

For a failure, first identify the issuing owner and the first violated contract.
Do not have everyone patch the final error. Separate product defects, obsolete
fixture premises and test infrastructure failures. Reproduce the claim through
the actual production entry point. Retain the useful refusal when replacing a
test that assumed custody granted execution.

## Resources: many agents, bounded builds

R resources inventories top consumers and active output users, then proposes
scoped retirements under the server guide. Preserve source, WIP, Git metadata,
evidence, selected standalone products, credentials and live runs. Inspect exact
jobs/leases/owned daemons; use supported archive/retire commands only after the
required retention and idle checks. No arbitrary file deletion inside Buck
outputs, broad cache clear, shared-service restart, or silent admission bypass.

The last 102 GiB scratch observation leaves only about 2 GiB beyond the reserve.
Recheck reservations and expected growth before every heavy admission. Use
`swarm-build` and the pinned local-only, lease-aware just workflow. Configure the
actual revision, including browser dependencies for exomonad-host/bundles. An
output mount or free RAM alone is not build admission.

Permit parallel focused lanes once measured aggregate RAM, cgroups, pressure
and disk growth support them. Read-only analysis, code editing and source review
can proceed meanwhile. Do not queue all tests behind an artificial global
single-lane rule, nor launch redundant cold builds to occupy workers.

## Finish and escalation

Run the order in [V](V-qualification.md), then update map/ledger and publish the
candidate with exact qualification. Do not claim main is clean/passing without
actual containment, remote-ref and gate evidence. Main promotion follows the
existing integration policy; publishing a labelled candidate is useful earlier.

If a strategy decision genuinely exceeds the accepted design, finish all other
unblocked work and consolidate it. Return a minimized counterexample, exact
source/artifacts, the violated invariant, alternatives, recommendation and the
remaining tests. No looping escalation tree. Ordinary implementation and fixture
repairs within the contract should be completed autonomously.

## Assignment / completion template

Use these fields in each worker's task file and returned report:

```text
Role and parent:
Base / checkout / branch / contract version:
Primary outcome and owned paths:
Adjacent review questions:
Known counterexample and evidence:
Dependencies; first action; escalation condition:
Acceptance commands and meaningful positive/refusal controls:

Candidate tip and cumulative diff:
What changed; owner/dataflow; APIs removed:
Exact commands, selected/executed counts, outcomes and artifact hashes:
First failure or unqualified boundary:
Review findings and disposition:
Resource/process cleanup certainty:
Incidental findings: confirmed / hypothesis / intentional exception:
Next owner and action:
```
