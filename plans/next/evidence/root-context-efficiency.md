# Context-efficiency follow-up

User requirement: each lead and its descendants should note context-window waste,
especially Haskell-tool discovery and bookkeeping, and propose UX fixes before
next wave. Collect concrete examples and distinguish firsthand observations from
inferred savings. Prefer small fixes in owning prompts, shared in-system API
guide, focused docs or tool behavior; do not duplicate a verbose activity log.

## Root observation

The root attempted exact active-request amendments to all three first-wave leads.
All three returned UpdateNotPresented:
`connecting update proxy: app-server closed the connection before responding to initialize`.
Retained workbench evidence: serviceCtxUpdate/runMapCtxUpdate/usageCtxUpdate and
serviceCtxState/runMapCtxState/usageCtxState. Request IDs 1, 2, 3; update sequence 1.
No lead receipt or incorporation is established. No queued-assignment fallback
was attempted. This is delivery UX friction, not measured token/cache evidence.

Outstanding: collect retrospective notes from each retained lead once its active
assignment settles; propagate to remaining work using a confirmed route. Include
observed missing documentation, avoidable discovery/administration turns, and
small proposed owner-specific fixes. Review actionable fixes before launching
the next wave. The service transport replacement already owns this delivery bug;
do not add another steering channel as an incidental workaround.

## Root priority: recursive self-improvement

User clarified that root's primary job is evaluating the team's use of Tidepool
and feeding that evidence into improvements and fixes. Product work continues as
real consumers and acceptance cases, not as a reason to overlook harness defects.

Root loop: observe retained execution/failure evidence; distinguish symptoms from
verified causes; assign a fix to the existing owner; review the exact candidate;
verify the integrated fix through realistic use; update owning guidance when the
lesson is durable. Do not infer successful improvement from source changes alone.

Current priorities:
- Worktree custody: two run-map children failed before implementation; other
  descendants are active. The service custody subtree is working, and root queued
  a separate regression-validation obligation. This is not universal fork failure
  or proof of a specific historical race. Require deterministic readiness/denial/
  cleanup checks and a real recursive-fork check on the corrected host.
- Active amendments: three confirmed NotPresented outcomes demonstrate that live
  steering is unavailable on the current route. Service replacement owns repair;
  do not silently replace amendments with queued assignments.
- Discovery/bookkeeping: collect lead and descendant firsthand examples, assess
  which missing shared guidance or tool behavior caused repeated work, and apply
  narrow owning fixes before subsequent waves. No token savings claim without
  measurement.

Preserve user-owned restart and external Codex-session boundaries. A partial run
that exposes a harness defect is useful evidence; addressing the defect remains
an outstanding obligation, not a successful product acceptance.

## Firsthand progress-setup discovery friction

Service reported no root escalation handle in its inherited context. Root added a
watched progress channel to its continuation (decisions in d59126b8). Setup exposed
another concrete documentation gap: `:doc watch` describes
`requestWithProgress actor options` but omits construction of `options`.
`:info RequestOptions` displayed a data constructor that is not exported for term
use; attempting it failed locally before any request was submitted. Compiler hint
then led to `:type requestOptions`, and the exported smart constructor worked.
Retained consumer: queueServiceContinuation, serviceContinuation/serviceProgress,
serviceProgressReady. Avoidable work was constructor inspection plus one rejected
input and an extra signature lookup, not a measured token estimate.

Proposed owning fix: show a complete `requestWithProgress` example using
`requestOptions label input` in the shared guide / focused watch docs, and include
watched progress as an explicit lead-assignment option when nonterminal findings
matter. Do not expose the private constructor merely to match misleading discovery.
Review alongside usage lead guidance changes to avoid overlapping edits.

## Run-map follow-up findings and evaluation lens

Attributed lead observations at fd6f1358d7f5b488d70f1cca6ef7caea661e6984:
- Shared unfold/watch guide was sufficient; retained named handles avoided
  assignment rediscovery on wake.
- Inherited root-looking context required one :status! to resolve child identity.
  Investigate activation placement/clarity rather than adding routine inventories.
- Permission failure receipts did not identify the denied operation/bootstrap
  stage, forcing source investigation. Existing custody owner should add bounded
  structured denial evidence, not another diagnostic registry.
- Nix shell setup stdout contaminated machine-readable report capture; direct
  execution of the built example produced clean JSON. Assess the shell-output
  owner before blaming the report parser or creating an alternative launcher.

Fresh reviewer is assigned the concrete partial-reader candidate, its retained
implementer and focused failure/evidence checks. It is not a retry of the two
failed implementation launches, and full run-map acceptance remains outstanding.

User's evaluation lens includes architectural potential: compare observed current
performance, capabilities of the intended low-friction system, and concrete distance
between them. Shared-prefix forks, parallel execution and retained specialist repair
are hypotheses for lower context cost and critical-path latency. Distinguish fixable
UX/mechanism defects from architectural limits; anticipated gains remain hypotheses
until matched revision/provider usage and accepted-outcome measurements support them.

## Expressive compression, separate from prefix caching

User clarified that Haskell's composition itself is a key advantage: map,
traverse/sequence, folds, closures and sum types can express useful coordination
with fewer tokens and less repeated state explanation than simpler tool surfaces.
Evaluate model fluency and conceptual leverage separately from provider cached
input. Retained typed repair/watch flows in the fresh run-map review worked
without rediscovery (reviewer report); root also observed that build/setup latency
can dominate even when coordination is fluent. Seek small reliable primitives
and enough guidance to compose them, not a bespoke bulk API or larger default
prompt for every workflow. Claimed token savings still require measurement.
