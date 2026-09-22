# Proposed Exomonad commit-and-fork operating model

This is a proposed operating model from the user interview, not an implemented
capability or an API commitment. It is separate from the STG wave trial parcels.

## Where delegation pays

Delegate when a compact specification expands into substantial work: reading
many files, applying a settled migration across callers, implementing tests
from a contract, chaining verification commands, or independently reviewing
an implementation. The parent supplies decisions and acceptance criteria;
the worker supplies execution and evidence. Luna can recursively delegate
when the same relationship holds inside its assignment.

Keep infodense planning with the parent. Writing the plan is often the work of
settling shared semantics, dependencies, ownership and tradeoffs; these cannot
be compressed into a short assignment without losing the very decisions the
plan must preserve. Having the parent dictate an entire file for a worker to
write merely repeats the output and adds a handoff. Mechanical documentation
updates from settled facts are delegable; authoring the design is not.

Optimize the parent's total specification, correction and review burden, not
the number of spawned agents. A worker's useful contribution must exceed the
cost of explaining and checking it. When a parcel needs nearly its entire
answer in the assignment, change the decomposition or do that part directly.

The parent can establish the contract as code: shared types, interfaces,
invariant-bearing entry points and minimal scaffolding. Children then implement
the mechanisms and migrate real consumers against that concrete checkpoint.
This is often less ambiguous than a longer prose specification. Natural-language
interaction remains available for exceptional cases; automation handles the
standard protocol rather than trying to replace judgment.

## Execution forks

An execution-fork operation would accept a branch name and create a tiny,
ephemeral worktree alongside the conversation snapshot. The branch/worktree is
an execution aid, not a second source of truth. Most implementation agents use
isolated worktrees. Same-directory agents are reserved for integration,
boilerplate, or a quick review where isolation would add more friction than
value.

Fork prompting should advertise “add all and commit” as part of the operation,
possibly through ordinary bash-mediated Haskell effects. Frequent commits are
units of work and handback points, not human review events; the final PR is
squash-merged. A machine-generated commit message can carry the fan-out plan
and durable handback: intended work, checks actually run, and known gaps.

Fan-out of two or more workers is the convention when work genuinely divides;
recursive Luna delegation is encouraged when a child has a meaningful
independent obligation. The parent remains responsible for integrated
correctness, regardless of where candidate commits were produced.

Each fan-out batch makes one shared checkpoint commit containing the batch
plan; every child starts from that same SHA. Results merge back onto the
parent branch descended from that checkpoint. Commits remain immutable: later
sibling integration advances the parent head rather than changing the fork
baseline. Notes may travel in the same implementation commit; this project
does not require a separate documentation PR or commit for interview notes.

Typed messages should primarily carry candidate or review SHAs and make the
state transition explicit: published, reviewed, integrated, or verified. A SHA
or a message receipt is not by itself proof of integration or verification.

## Adversarial review and runtime boundaries

Worker handback includes a required quick fresh-context Luna adversarial
spotcheck. It shares the worker's environment/directory rather than creating
another worktree, is instructed to be read-only and perform no builds, and
receives the specification from the root. The worker's commit message is a
durable account of intent and evidence, not an independent acceptance claim.

A typed rubric routes findings by what must change: small implementation
issues return to the worker for repair; a misunderstood assignment, conflicting
contracts or an unsettled design decision returns to the parent. A clean
spotcheck records no blocking finding, not proof of exhaustive correctness.
This keeps local repair inexpensive without letting a worker redefine its
assignment. The parent still owns the integrated result, not merely a bundle
of child summaries. The exact rubric constructors and repeat-review policy
remain to be specified.

Notifications may accumulate after a tool call without waking paused
inference; consumers must not treat prompt wake-up as delivery or completion.
Build leases are independent of worktree isolation: a private worktree does not
grant a private build lease, and a shared build lease does not require shared
source files.

## Lifetime and integration coordination

Luna workers are ephemeral. Dispose of their agents and worktrees after
handback; keep candidate commits reachable until integration or explicit
abandonment. Astra/Sol owners are longer-lived and remain available until
explicitly shut down. Agent/process lifetime, checkout lifetime and durable
candidate reachability are separate concerns.

Advancing the parent head can make sibling candidates need a rebase. Express
that coordination through the existing Haskell effects and messaging framework:
route which candidate needs rebasing onto which exact new baseline to the
responsible owner, and report the replacement candidate SHA and actual checks.
Do not confuse sending a rebase request with the rebase having completed.
Mechanical integration can be delegated; conflicting semantics return to the
parent rather than being resolved by an automatic policy.

This is ordinary orchestration code, not a reason to add a separate messaging
or supervision system. Headless Codex workers are a possible execution option,
not a verified requirement of this design. Microagents can instead run in the
owner's existing interactive environment while using separate worktrees.

## Details left to implementation

- Exact typed review outcomes and orchestration-level retry limits, rather
  than baking a fixed retry count into the fork API.
- Reference retention and cleanup mechanics that implement the lifetime rule.
- Rebase notification delivery/acknowledgment details and execution backend.

The operating model above is settled by the interview. These implementation
details still need checking against the owning source before shipping; the
note itself adds no runtime authority or new coordination framework.

## Lessons from the Luna-heavy STG trial

These observations describe an in-progress trial, not a cost benchmark or a
completed engine migration.

- Workers performed the implementation and caller/test migrations; the parent
  spent its effort on the common contract, unsafe/semantic review and repairs
  to misunderstood premises. Infodense plan authoring stayed with the parent
  after one mistakenly delegated draft illustrated the wrong decomposition.
- An informal build slot cost parent messages to acquire, grant and release.
  Some workers ended their handback with required checks still pending; another
  interpreted "slot is free" as permission. Availability, acquisition and
  completion need distinct typed events, not conversational interpretation.
- A deletion worker regenerated the lockfile correctly as a tool operation but
  upgraded unrelated packages. The repair preserved pinned versions and pruned
  only unreachable packages. Verification must check the intended delta, not
  merely command success. This correction was delegated to Terra.
- The first collector implementation passed its focused tests while rebuilding
  the per-object metadata/Arc machinery the design explicitly removed. Review
  also found a forwarded-indirectee failure path and scratch-reservation issues.
  Test success is not contract acceptance; unsafe/global-property review is
  still judgment work, even when a cheap reviewer helps identify findings.
- Another fresh-context review identified diagnostic precedence changed by
  global top publication. Its useful output was an exact conflicting case for
  the parent to decide, not an autonomous rewrite of validation policy.
- File isolation and commits would make baselines and handbacks easier to
  attribute. Shared-tree completion currently requires coordinating overlapping
  files, build ownership and partially finished changes independently.

No trustworthy planner/worker token split is exposed in these handbacks.
Routing automation would remove repeated clerical turns, not the shared design
or semantic review. Do not turn that expectation into a measured cost claim.

## Desired Haskell orchestration surface

The DSL should let an owner state a small workflow over typed obligations,
commits, candidates and review outcomes. The following names are illustrative,
not a committed API:

```haskell
-- One checkpoint and shared plan; isolated children use that exact baseline.
batch <- forkBatch plan [(branchName, obligation), ...]

-- A worker's normal path; the lease brackets the real check process.
evidence <- withBuildLease (verify acceptance)
candidate <- publishCandidate evidence
review <- spotcheck rootSpec candidate
routeReview review
```

The important capabilities are behavioral, not these spellings:

1. **Checkpoint and fan out.** Commit the advertised source snapshot and plan
   once, return its SHA, and create children at that exact baseline. Failure
   after checkpointing must report which children actually started. Do not
   imply an all-or-nothing fork if only some child processes were admitted.
2. **Acquire and release resources.** Queue build requests and notify the holder
   automatically. A bracketed lease releases after the check process stops,
   including failure/cancellation. Agent death alone is not proof that its
   compiler child has stopped; process supervision must establish release.
3. **Publish evidence.** Keep the handback in the candidate commit message:
   intent, changed contract, actual commands/results and unresolved issues.
   Typed notifications carry references and outcomes rather than reprinting
   that account. Pending required checks are an explicit incomplete state,
   not an accepted candidate.
4. **Review and route.** Run the required read-only/no-build spotcheck with the
   root spec. Route a bounded local repair to its worker and a premise or
   contract conflict to the parent. A terminal worker can be replaced by a
   fresh worker at the candidate SHA with the review attached; keeping its
   entire conversational history alive is not required.
5. **Fold and rebase.** Integrate exact candidates, verify the resulting
   revision, and automatically notify affected siblings when their required
   baseline advances. Preserve the old candidate as evidence and return a new
   SHA after rebase. Reviews and verification attach to exact revisions; stale
   evidence cannot silently approve a different candidate.
6. **Escalate exceptions.** Deliver the obligation, baseline/candidate SHAs,
   concrete failure and attempted repairs to the parent or stronger worker.
   Leave natural-language messages available for everything the standard
   state machine cannot represent usefully. Do not force design discussion
   into strings that secretly drive control flow.
7. **Notify without forced inference.** Queue ordinary state changes and
   after-tool-boundary messages without waking a paused owner for every event.
   The workflow can continue standard transitions while the parent sees
   exceptional decisions at its next appropriate inference boundary.

This is a typed state machine around agent work, not a tree of smart agents
manually forwarding each other's messages. Published, reviewed, integrated and
verified remain different states. Root/source/process authority stays in the
existing Rust interpreters and owners; Haskell composes policy using ordinary
effects. Reuse the existing git/worktree, messaging and process-supervision
mechanisms instead of adding parallel registries or launchers.

### Recursive worker/reviewer loop

An owner should be able to submit the specification and acceptance criteria
once, then let the Haskell workflow run a Luna subtree. A worker publishes a
candidate, a fresh reviewer returns a typed rubric outcome, and the state
machine routes a local issue back to that worker or a replacement at its SHA.
Only a misunderstood obligation, unsettled contract, exhausted repair policy
or other exceptional condition needs the parent to resume and decide.

Apply the same loop at every recursive merge level. Child candidates are
integrated and the resulting revision is reviewed against the enclosing
obligation; individually reviewed children do not imply a reviewed integration.
The root contract remains shared evidence, with narrower child obligations
attached rather than independently paraphrasing away its invariants.

The reviewer supplies judgment; typed Haskell control flow automates routing,
iteration and completion checks. Keep candidate/review/evidence SHAs correlated
through repairs, and require any configured build acceptance separately from
the no-build spotcheck. This avoids implementing the routine protocol as a
second swarm of orchestration agents while retaining natural-language contact
for decisions the rubric cannot settle.
