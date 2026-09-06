# Recursive context collaboration as a core Shoal use case

Status: accepted product direction; prompt artifacts and a partial implementation
are drafted, not runtime-verified. Start implementation with the
[focused handoff](recursive-context-collaboration-handoff.md). The immediate
scope is preparing Tidepool/Shoal before starting the `shoal-repl` project.
This document defines the next core use case. Writing it does not establish
that peer authorization, executable acceptance transfer, or the complete live
workflow has been verified. The existing
[recursive scaffold campaign](recursive-scaffold-campaign.md) supplies the
scaffold, obligation, and integration contract. This plan extends that campaign
and replaces its broad comparative-experiment direction with improvement through
useful work. Current runtime semantics remain in [SHOAL.md](../../SHOAL.md).

## Objective: improve recursively while doing useful work

Make scaffold / fork / fold / repeat a natural working rhythm at every level
of Shoal. A node develops shared understanding, commits a concrete scaffold,
forks independent obligations, folds code and selected evidence, and repeats.
Children can use the same rhythm internally. Retained actors, useful Haskell
definitions, acceptance functions, and improved scaffolds make subsequent work
cheaper and better informed.

The user wants iterated recursive self-improvement through actual development.
Broad benchmark campaigns, repeated matched-task trials, compulsory extra
features, and expensive provider experiments are not prerequisites. Use Astra
for the initial recursive application campaign. Smaller coding agents can finish
the bounded runtime and verification preparation in the handoff. Observe cost and elapsed time using existing
observations during useful work; do not purchase a separate experiment program.
Self-improvement here means improving working practice and reusable artifacts,
not modifying model weights or requiring a generic self-modification framework.

## Why the context tree matters

Consider six independent medium-sized tasks. Sequential execution adds the
implementation history of each to the context used for subsequent tasks.
Scaffold once and fork instead: each child inherits the shared foundation,
then accumulates only its own implementation detail. The parent folds back
code, evidence, decisions, and consequential discoveries. Debugging detours,
failed commands, and local exploration stay in retained specialists.

This can improve speed and token use together: independent work runs
concurrently while unrelated implementation histories stop accumulating in
every later task. Shared-prefix caching can further reduce input cost where
the provider actually supplies it. These are mechanisms and intended benefits,
not a claim of measured savings in this session.

The aim is to linger productively in a valuable high-context interval—roughly
X00k to Y00k tokens in the user's description—by branching from a mature shared
prefix instead of pushing one conversation through all the work into
compaction. These are illustrative positions, not recommended thresholds.
A large coherent context can be valuable. Preserve it, control what is appended
to it, and let descendants develop more specialized mature contexts in turn.
Forking does not freeze the parent or prevent eventual compaction; selective
folding determines how much its history grows.

Tree depth and width follow the work. Fork when shared decisions are settled
enough for independent progress and the next steps will generate detail that
belongs with an implementer. Recurse when that implementer discovers another
independent frontier. There is no minimum ticket size, mandatory branching
factor, or universal depth. Fine obligations can deserve an Astra context when
the scaffold makes them clear. Actors already use Python and shell scripts for
mechanical work; this plan does not need to teach that distinction again.

Context inheritance and supervision form a tree. Authorized collaboration may
cross branches through typed actor requests. The tree determines where context
originates and who owns work; it need not force every message through a parent.

## Coding actors can recurse

Coding is an activity, not a leaf-only authority class. A coding actor should
be able to scaffold, fork, observe, repair, and integrate within the parent's
remaining depth/width budget and selected effect row. A parent should not have
to predict in advance which implementation task will need recursive thought.
An explicitly narrowed actor or exhausted budget can still make it a leaf.

The draft gives `CodingEffects` the existing recursive coding capabilities,
shares that row with `ScaffoldEffects`, and derives descendant attenuation from
`Forks` membership at launch. The scaffolding role remains a prompt emphasis,
not the sole entry to recursion. No serialized role names are removed. The
handoff identifies the authority and compatibility checks still required.

## Canonical scenario: one interface, four independent responsibilities

The parent resolves the shared interface, its behavior, examples, ownership,
and enough build wiring to expose independent implementation obligations. It
commits this scaffold before capturing the children's worktree seed.

| Branch | Responsibility | Useful local context |
|---|---|---|
| Pure test implementation | Implement a deterministic in-memory implementation or test double behind the interface | Fixtures, controlled behavior, consumer testing needs |
| Integration tests | Exercise the real implementation through the interface | Observable contract, real dependencies, failure and cleanup behavior |
| Real implementation | Implement the underlying mechanics | Storage, transport, resource lifetime, debugging |
| Consumer | Build application behavior using the interface | Application semantics and user-facing behavior |

For example, the integration-test coordinator may find three independent
responsibilities: normal behavior, failure handling, and resource cleanup. It
scaffolds the shared fixtures and test entry points, then forks one child per
responsibility. Those three histories remain below the testing coordinator;
their accepted result fulfills one testing obligation at the parent boundary.
The real implementation can independently recurse over its own interfaces.

The four branches need not reach the same completion stage at the same time:

- Consumer code can compile and run against the pure test implementation.
- Integration tests can compile while failing against an unimplemented real
  implementation. They must eventually run against that real implementation.
- The real implementation can expose compile-correct interfaces with explicit
  unimplemented operations, then discharge them recursively.
- A coordinator can accept partial internal fragments while still owing a
  complete component to its parent.

Compile-only acceptance is valid when explicitly assigned. It is never evidence
of runtime correctness. Tests that compile but have not run, tests failing at
an expected stub, and passing tests are different observations.

Use named TODO/completion markers and a small project manifest when they help
identify holes. Record required behavior, the owner, and permitted remaining
obligations against exact revisions. Tags or commit trailers can make partial
commits visible. Marker removal alone does not prove completion, and a stub
must fail visibly rather than produce plausible successful behavior. Do not
build a universal marker parser or impose one completion ladder on every node.

The fold assesses whether all four perspectives actually fit the interface.
An awkward consumer workaround or an integration test unable to observe a
promised behavior is evidence for revising the scaffold. Preserve such findings
even when the local implementation otherwise meets its assignment.

## Review from current understanding, repair directly with the implementer

The root should ordinarily retain a compact sequence of meaningful events:
issued contract, received candidate, review verdict, accepted revision or
escalated decision. It should not have to absorb implementation and inspection
histories to relay requests between specialists.

1. The parent forks implementation from the committed scaffold and the
   understanding available at that point.
2. While implementation proceeds, the parent can make other shared decisions
   and integrate independent work.
3. The implementer submits a specific candidate and settles that implementation
   request. The actor remains available with its specialist context.
4. The parent forks a reviewer from its now-current context, selecting the
   candidate to inspect and supplying the issued contract, relevant decision
   delta, and implementer actor reference with the required authority.
5. The reviewer inspects that exact candidate. It sends typed repair findings
   directly to the retained implementer and watches the repair response.
6. The implementer repairs in its own worktree and returns a new candidate and
   evidence. The reviewer checks that candidate and repeats as needed.
7. The reviewer settles its parent-facing request with acceptance or a precise
   decision requiring parent judgment. The parent verifies the acceptance
   evidence and performs integration and the checks required at its boundary.

The reviewer inherits current parent context, not the implementer's entire
history. A fresh reviewer is an inherited-context fork here, not necessarily a
fresh-context worker. Current understanding can reveal that a contract needs
revision; it does not silently rewrite the contract originally issued.

The parent delegates getting a candidate accepted under an agreed contract,
not just producing a patch. The implementer owns edits, the reviewer owns its
verdict, and the parent owns contract changes and acceptance into its work.
Reviewer independence is compatible with direct discussion and repair.

### A typed local protocol, with one driver

Project-authored sums and records can distinguish, for example:

```haskell
-- Schematic project types, not proposed runtime exports.
data ReviewOutcome
  = Accepted Candidate ContractRevision Evidence [Discovery]
  | NeedsRepair Candidate ContractRevision [Finding]
  | NeedsDecision Candidate ContractRevision Decision

data RepairOutcome
  = Revised Candidate Evidence [Discovery]
  | RepairNeedsDecision Decision
```

Candidate values identify exact commits. Findings identify observed behavior
and required corrections. A decision explains the mismatch, relevant evidence,
and what the parent must decide. Runtime failure and cancellation remain
lifecycle outcomes, not successful domain replies. Acceptance must identify the
reviewed commit and contract; a changed candidate requires renewed review.

The reviewer drives the loop. Each repair request settles before the reviewer
issues another. An implementer needing clarification returns a typed decision
or clarification need so the reviewer can answer in the next request. Do not
create a circular wait where the reviewer awaits repair while the implementer
awaits a new request on the already-occupied reviewer. Actor request scopes are
serial even when watches allow them to span model turns.

Keep the reviewer's original parent-facing request pending across repair
watches. Register a watch and end the model turn; on activation, inspect typed
state and continue. This uses existing requests and watches, not a nested
scheduler. The parent watches the review outcome rather than every exchange.

Within-contract mistakes should be resolved locally. Escalate changed scope or
interfaces, incompatible requirements, missing authority, or a repair loop that
cannot make progress. Let the assignment set any needed bound on repair effort;
do not impose a universal iteration count. Preserve useful commits and concise
findings when escalation or failure occurs.

### Sharing references does not share all authority

Verify sibling request admission and runtime grants for this concrete topology.
An `AgentRef` naming the implementer is distinct from the parent's `Response`,
`Reply`, or `Watch`; the reviewer creates and owns its own repair responses and
watches. Do not assume inherited observation or settlement handles are usable
by another actor merely because they appear in its Haskell scope.

The reviewer needs its own permitted checkout and execution authority for checks.
An inspection-only role cannot run tests. Requests do not confer permission to
write the implementer's worktree. Preserve one code owner and inspect immutable
candidate commits through the existing worktree mechanisms. Extend the owning
authorization boundary only if this use case exposes a missing capability.

Cross-branch communication does not reparent either actor. Cancellation must
cover the owned work that was promised: cancelling a reviewer request alone
must not be presented as proof that a sibling repair stopped. Use existing
request/lifecycle observations and subtree control to establish that truth.

## Executable acceptance and a possible bash quasiquoter

Resident Haskell lets an actor contribute a way to judge code as well as code.
“Ship me an acceptance function” can mean a live function whose conceptual
shape is:

```haskell
-- Illustrative shape; Candidate, Contract and Acceptance are project types.
-- Actual signatures declare the Member constraints for the effects they use.
acceptance :: Contract -> Candidate -> Eff effects Acceptance
```

Taking an explicit candidate lets the parent apply the check to the integrated
revision. Taking a contract explicitly avoids accidentally accepting under an
older captured policy. Pure predicates, functions constructing effectful checks,
and composed checks can become resident values shared with other actors.
A monadic action is useful where sequencing matters; a parameterized function
is usually more reusable than a closed action capturing one old checkout.

The proposed `[bash| ... |]` quasiquoter is a lightweight bridge from familiar
shell practice into this vocabulary, analogous in convenience to `fmt`.
Desired properties: familiar shell syntax, defined interpolation that safely
represents argument values, explicit execution context, and a structured exit
status/stdout/stderr result. It should construct work consumed by the existing
Rust process owner. Haskell must not grow a process supervisor or shell parser.
Specify how shell source and interpolated data differ before offering execution;
ordinary formatting alone is not safe shell argument construction.

This is an enabling direction, not a prerequisite for recursive collaboration.
First inspect existing command effects and quasiquoters, then implement the
smallest useful bridge if real acceptance code needs it. Do not replace useful
Python or shell scripts with a mandatory DSL. The potential gain is making
improvised tools resident, typed, composable, and inheritable without repeatedly
reconstructing their usage from prose.

Live-value mobility, effect-row compatibility, and execution authority are
separate. Check the complete producer-to-reviewer-to-parent path before promising
effectful checker transfer. A received closure runs under the receiver's
authority; it does not borrow the author's privileges. Row-polymorphic helpers
should use `Member` constraints; do not invent a frozen universal effect stack.
See [live values and authority](live-values-and-authority.md).

The author of an implementation cannot redefine success by supplying a weak
checker. The reviewer assesses whether the check covers the issued contract,
and the parent remains responsible for its integrated result. Fold useful
acceptance code and discoveries upward alongside evidence, not just a boolean.

## Keep snapshots and learning explicit

Four things advance independently: conversation history, resident Haskell
bindings, issued contract, and Git revision. Children inherit the completed fork
tool result and final committed scope; already-selected inputs and worktree
seeds retain value semantics. Later parent changes do not update existing
children. Earlier closures retain their captured bindings after a name is
rebound. A retained follow-up therefore needs the decision delta and candidate.

Make important shared reasoning explicit in source, declarations, and concise
decisions before forking. Exact history inheritance preserves the explanation;
a compact assignment makes the important obligations easy to find. Do not
replace inheritance with repeatedly reconstructed briefs or assume unexpressed
model deliberation becomes a shared contract.

Fold consequential discoveries upward even through multiple levels. Successful
fulfillment can still reveal a bad interface or a new failure case. The parent
needs those findings to improve the next scaffold; it usually does not need
every command that led to them. Retain implementers and reviewers when their
local histories will help the next repair or feature.

Improvement is cumulative: useful project definitions, acceptance functions,
scaffold conventions, and concise agent guidance survive into later work. Add
structure when it resolves a real ambiguity; do not require a universal campaign
AST, obligation service, report schema, usage ledger, or completion ladder.
Live closures and handles are not durable restart artifacts. Promote reusable
source to repository files without claiming that doing so restores live state.

Improving the working environment is part of the task itself. Using tools to
improve tools is a core product bet: actors should address concrete friction
and develop useful helpers during ordinary project work, rather than always
deferring that work to a separate improvement queue. Larger architectural
changes still follow the project's agreed autonomy and proposal boundary.

One possible later home for reusable project vocabulary is a committed
`.shoal/Helpers.hs`, automatically included in the resident environment and
iterated on through normal development. This is a future direction, not an
instruction to add the file or auto-loading now. Let useful session definitions
and actual consumers establish what belongs there. Any eventual loading and
update design must make source revisions, declaration generations, and effects
on retained actors explicit; changing the file must not be described as silently
updating existing closures or assignments. Reuse the existing toolchain and
workbench owners rather than adding an independent loader.

Current `tidepool/src/shoal.rs` treats `.shoal/` as runtime-owned and installs a
local Git exclusion for the directory. A future committed helpers file needs
an explicit source-versus-runtime layout decision at that owner; the proposed
path is not already compatible with the current convention by default.

## Prompt delivery

The prompt artifacts teach the decision model; mounted help carries procedures.
Keep the shared prompt centered on three ideas: place knowledge deliberately,
make independent work concrete with scaffolds, and fold decisions and evidence.
A future actor should understand why to fork, what to preserve, and where to
find the next operation without inheriting a manual in every role prompt.

Role prompts supply only their emphasis and constraints. The root owns technical
execution and user collaboration. Coding actors can implement and recurse;
scaffolding emphasizes interface preparation. Review is an assignment that can
include direct typed repair, not a new universal runtime role. Inspection-only
actors remain honest about their inability to execute checks.

The edited artifacts are `prompts/shoal/tree-practice.md`, the existing role
prompts, and `prompts/shoal/docs/{tree,unfold,refinement,watch,workbench}.md`.
`tidepool/src/actor_host/prompt_catalog.rs` owns role/shared artifacts and their
catalog version; `tidepool-actor/src/prompt_catalog.rs` owns mounted help and
hosted-tool text. The host combines the effective developer prompt fingerprint
with the hosted-tool fingerprint. Keep these owners and update their actual
consumers rather than adding another prompt layer.

The [handoff](recursive-context-collaboration-handoff.md) records draft status and
executable checks. Source edits do not change an already-running host's prompts,
Haskell snapshot, or runtime authority.

## First useful project: shoal-repl, an operator workbench TUI

The selected first substantial project is `shoal-repl`, a clean, highly polished
terminal pane that runs in tmux, built fresh as a standalone Rust crate. It does not build
on or replace the existing `shoal-console` signal-lab project. Give the operator a
persistent GHCi-style workbench comparable to the one actors inhabit: evaluate
Haskell, retain definitions, message agents, use system capabilities, and inspect
results. This is an interactive working surface, not primarily a dashboard.

The TUI is a thin client. It holds and presents interaction history, edits
input, and renders beautiful, syntax-highlighted text. Haskell evaluation,
session state, actor routing, authority, and runtime meaning belong to the
host's existing owners. Syntax coloring is a presentation concern; the client
must not parse rendered Haskell to infer execution success or actor lifecycle.
A standalone build must not require embedding GHC, the JIT, or the actor runtime
inside the presentation crate.

Visual quality and interaction polish are part of the first useful outcome.
The agreed input experience is a comfortable, expressive multiline composer
with basic history scrolling. Prefer existing text-editor crates to DIY editor
machinery; apply the same reuse philosophy throughout. Readable diagnostics,
beautiful syntax-highlighted results, asynchronous arrivals, and resizing within
tmux should feel coherent.
Do not defer all of this behind a merely functional command box. Detailed
interaction choices remain part of the product conversation; no UI framework
or implementation layout is selected by this plan.

The architectural boundary to settle first is the operator session: its own
identity, bindings, authority, lifetime, and access to actor references and
results. The initial recommendation is a separate operator workbench using the
same runtime substrate, with explicit communication with agent workbenches.
Confirm that choice with the user rather than assuming the operator edits a
root actor's live scope. Distinguish client history from authoritative runtime
state, and client reconnection from recovery after a host crash.

Repository inspection found existing resident workbench/session mechanisms in
`tidepool-actor/src/resident_workbench.rs` and `tidepool/src/actor_host.rs`.
This is source-inspection evidence, not a verified operator attachment path.
The operator client starts in a fresh standalone crate; the separate
`/home/inanna/dev/shoal-console` signal laboratory is outside its implementation
scope.

The immediate task is a substantive review and refinement of the Tidepool/Shoal
prompting artifacts, a complete implementation handoff, and a `just shoal-repl`
launcher. Preserve the drafted code for coding agents to finish and verify. The
user will steer the TUI as it is built; do not start that build in this preparation
phase. Once preparation is accepted, build one real operator interaction end to
end. Establish persistent
input/evaluation, an agent inspection or typed request, and clear presentation
of its eventual result. Extend missing host behavior at its owner. Avoid a broad
repository-improvement phase before useful TUI work; necessary environment fixes
are part of delivering the slice.

This project is itself a natural scaffold/fork/fold consumer. Once the client /
host boundary and interaction contract are settled, independent obligations
can cover a deterministic backend for UI development, real host integration
tests, the host adapter, and the presentation client. Tests can split by distinct
responsibilities. Review can fork from the parent's newer product understanding
and repair directly with the retained implementers. Let actual dependencies
determine that tree instead of imposing these branches before the scaffold exists.

## Implementation sequence and acceptance

### Ongoing product conversation and everyday usability

The desired root experience is an autonomous high-level collaborator, similar
to a staff developer working with a technical project manager, product owner,
and tech lead. The root owns technical execution and coordinates the tree;
the user receives concise decisions and outcomes, with details available on
request. The conversation should preserve product intent, architectural
reasoning, and consequential findings without requiring the user to manage
actors or follow routine implementation traffic.

Autonomy depends on the project and agreed level of initiative. For a major
architectural proposal, investigate enough to present considered, curated
options and a recommendation before treating the change as settled. The root
may also pause to ask about development philosophy when that answer would
guide multiple concrete decisions. Such a question need not be tied to one
immediate implementation choice. Do useful investigation before escalating;
do not turn routine work into repeated approval requests.

A possible future TUI control dashboard is an operator-facing product choice.
The LLM's conversation should not imitate a control room or force the user to
monitor the tree. The system is too new to assume a history of user frustrations;
use forward-looking scenarios and firsthand observations as well as any actual
friction that emerges.

Develop this direction with the user through an iterative interview and actual
use. Everyday jank and bad UX are in scope whenever they make Tidepool/Shoal
harder to use skillfully: discovering operations, expressing Haskell, finding
retained values, understanding activity and authority, inspecting evidence,
recovering from rejected units, steering work, and keeping the user informed.
Joy and effectiveness include how readily an actor can express an idea and
recover its bearings, as well as the final code it produces.

Ask about concrete frustrating moments and the desired root/user collaboration
style. Bring observations from inside the live harness, distinguish observed
behavior from hypotheses, and develop improvements together. Keep questions
focused and record resulting decisions here or at their existing owner. Do not
turn the interview into a broad measurement program or another mandatory
workflow. Fix relevant friction at its owner as implementation proceeds.

### Preparation and application acceptance

Complete the [focused handoff](recursive-context-collaboration-handoff.md) before
starting the application campaign. That document owns the preparation sequence,
implementation gaps, and exact checks. It can be completed by smaller coding
agents without re-deriving the product direction or running a live swarm.

During subsequent useful application work, seek a natural recursive split,
incremental integration, review from newer parent context, direct repair, and a
compact verdict. Do not manufacture extra features or failures to fill a matrix.
Retain useful helpers and improve them during the next ordinary cycle.

Acceptance requires evidence that the selected commits meet the issued
obligations, final required holes are discharged, and integrated checks pass.
Report exactly what ran, what only compiled, and what remains unverified. Also
verify the supported repair topology's failure and cancellation paths through
focused tests; a domain `Accepted` value is not lifecycle evidence.

The delivery should leave the parent with contracts, candidates, reasoned
verdicts, and consequential discoveries; local implementation and review
histories remain with their actors. This is the concrete context-management
behavior to establish. No prescribed context threshold or measured speedup is
an acceptance gate.

As behavior lands, move proven practice into the prompt/help owner and project
guide, and runtime invariants into their owning documentation. Retire this plan
when its implementation and prompting obligations are fulfilled.
