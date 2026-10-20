# Complete source lifetime, selective retirement and recovery

Status: implementation plan, not execution or release evidence. Read the
[campaign plan](../plan.md) and [locked contracts](../contracts.md) first. This
area starts from joined revision `6f7ae09b3bdbcfd8952d9a513db91564ea1b5858`
(code tip `92fa518105`). At kickoff record the actual receiving revision and
review intervening code. Existing lifecycle handoffs are historical evidence;
their qualification claims do not transfer to a new candidate.

The area has two bounded leaves. Source/custody establishes the admission and
release contracts used by the new compiler-service route. Retirement/recovery
composes those contracts through the existing reload, directory, operation and
publication owners. Deliver the source/custody boundary early; full forced
retirement and recovery remain mandatory campaign work after the early Persona
gate. No supported finite-cell fallback remains in either delivery.

## Joined facts and repair boundary

The join already has stronger preview admission than the stale
`feature/shared-warm/reload` tip `77e0f50f7a`: `RuntimeActivationPreviewAdmission`
uses Fresh/Bound/Consumed, an atomic bind claim and a separate atomic spend.
`CompiledActivationPreview` carries the non-copyable bound token. Generic preview
installation refuses at the owning installer, and controls now enter public
authored-turn and snapshot paths. Typed spent-admission refusal and a two-thread
cached-renderer bind control also exist. Preserve these mechanisms. The latter
control lacks a deterministic barrier; extend its observation rather than replace
the state machine with the stale branch's Boolean.

The missing distinction is original source A versus target B. At this revision,
preview `source_lease()` returns its detached target lease. Admission does not
retain the original compiler transaction's source lease. The revoked-original
control retires that detached target, so it cannot prove refusal with A revoked
and B still current. Canonical-issuer validation alone also does not establish an
Open source gate. The new service issuer must retain original A explicitly; a
later caller's transaction or target B cannot substitute for it.

The join's `discard_custody` consumes a root and returns a Boolean; refused
physical release cannot return the same root with a typed cause. Deferred Drop
cleanup is still a backstop, so this observation alone is not a demonstrated
physical leak. Final activation publication also lacks the follow-up's complete
presentation/receiver/late-producer cleanup ownership and final original-source
check. These are concrete repair boundaries, not grounds for importing every
follow-up edit.

Force is already a schema/authored Boolean, default false, and currently refuses
before preparation. Ordinary reload prepares then publishes; it does not yet
perform old-generation fence/capture/default-blocker/drain. Candidate installation
fencing is not old-generation retirement. Existing generation dependencies,
source-ordered directory reservation enqueue, population observations, source
fences, shutdown owners and retained cleanup receipts are foundations to extend.
Population listing alone is not an atomic physical shutdown closure.

Sources for kickoff inspection include `tidepool/runtime/src/session/{admission,
source_admission,resident,turn}.rs`,
`exomonad/actor/src/resident_workbench.rs`,
`exomonad/actor/src/request/{activation,sources,routes}.rs`,
`exomonad/actor/src/resident_actor/{owned_workbench/reload,source_effect,
invocation_work,forms}.rs`, `exomonad/actor/src/termination.rs`, and
`exomonad/actor/src/local_actor/run_control/generation.rs`.

## Ownership and shared-file handoff

| Leaf | Owned result | Receiving dependency |
|---|---|---|
| Source and custody | Original A/target B admission, source-bound output, affine renderer bind/run, final root/receiver ownership and exact cleanup refusal | New service/output issuer agreed with service-cells; shared runtime edits received by runtime-publication before cutover |
| Retirement and recovery | Actual-set fence/capture, default blockers, explicit force drain, truthful partial settlement and exact recovery | Source/custody contracts plus executable new service output and existing coherent generation publication |

The lifecycle integrator owns the joined result and exact-candidate review.
Runtime-publication owns shared runtime admission/publication files. Send focused
commits defining constructors, invariants and affected callers to that owner; do
not independently rewrite the same constructors. The source lease must be captured
at the SAME new service admission/output issuer, including startup and renderer
consumers where applicable. Agree on this field and consuming signature before
parallel implementation. The lifecycle leaf does not revive finite preparation
to make a stale parcel compile.

Use the stale follow-up only as source material. Relevant bounded changes include
original-source retention (`9e8de8c48a`, `af665af30e`), sealed ordinary/bootstrap
preparation (`fa3db54efe`), typed custody refusal and primary-error preservation
(`77f40fae05`, `2fb214a10e`), and presentation/receiver ownership
(`5ed840b0b3`, `2b35423abf`, `3774698226`, `1e1c431474`). Inspect their complete
dependencies and current production consumers; port the missing behavior onto the
join rather than merge the 46-file branch wholesale. Its older preview installer
and fixture migrations are not an accepted replacement for current code.

## Wave 1: source and custody

### Issuance and use

1. Capture original A from actual admitted source work at the new service issuer.
   Retain its exact canonical lease separately from target B and compiler origin.
   Couple successful checked output to those owners; do not construct authority
   from world IDs, artifacts, equality, paths or the recipient's current source.
2. Extend the existing source-bound output/preparation owner. Generic ordinary
   installation requires that runtime-issued product; independently prepared
   bootstrap recipes use their existing production issuer and their SAME settled
   preparation/cancellation evidence. Eliminate raw ordinary output routes that
   omit this relationship. Compiler END uncertainty retains owners and refuses
   sealing, execution and publication; it does not authorize cold retry.
3. Enclose the renderer's bind claim with exact canonical-issuer validation and
   Open-gate admission for original A, target B and their actual prerequisites.
   Use existing source-admission/issuer owners and their established total lock
   order. No source lock spans compiler work, machine acquisition, external IO or
   an await. Proof compatibility is checked before claiming Bound; no last native
   owner is destroyed while these guards are held.
4. After off-checkout compilation or cache waiting, revalidate both A and B in the
   actual compiler transaction before consumption and installation. Cache identity
   never supplies fresh source authority. Preserve Fresh -> Bound -> Consumed;
   refusal before the spend leaves it unconsumed but does not reset Bound to Fresh.
   Dropping a refused bound invocation cannot reseal the admission.
5. Carry the same original source through callbacks and final activation/tool
   publication. Validate the exact original reply definition and request at the
   final source/request/publication boundary; recipient B or absent checkpoint
   metadata cannot issue replacement authority. Preserve an already committed
   native activation when later presentation refuses.

### Native release and disappearing waiters

Extend existing `ActivationPublicationResources`, installed/standing ownership,
request settlement and `RetainedActorExit`; do not add another teardown registry.
Enroll cleanup ownership before native import/allocation. Retain actual dispatch
roots, installation scopes, captured request receiver, parked holes and active
producers until the final slot accepts them or physical cleanup settles.

The same-session installer branch borrows the original root; releasing that borrow
must not discard its owner's live value. Cross-session import owns a new root and
retains it before escape. Receiver custody transfers only on actual standing or
execution acceptance, not on enqueue or return of an intermediate future. A late
producer remains counted even if its async waiter disappears; an empty payload
while a producer can still allocate is not cleanup success.

Known release refusal returns the SAME opaque root/receiver/lease with a typed
cause. Successful release returns an observation of the actual physical release.
Confirmed reader-busy refusal before release returns that same custody and a
settled typed cause; it is not unknown physical settlement. Panic during release,
lost acknowledgement and unobserved dispatched settlement remain explicit retained
uncertainty. Native cleanup authenticates physical ownership without
requiring fresh source execution permission. Preserve primary action errors and
secondary cleanup/END/publication causes together through actor replies and
receipts. Preserve the primary causal classification and each typed secondary
failure; determine overall success, retryability, admission and recovery obligations
from their combined outcome. A user-code failure cannot conceal infrastructure
uncertainty, imply confirmed cleanup or authorize replay. Release
redundant native holds only after source/request/installed/terminal guards unlock.

### Wave 1 acceptance

Use the actual new service issuer and public consumers. Existing preview tests are
starting points, not permission to keep legacy fixture authority. Required controls:

- Admit under A into a distinct live target B; fence or retire A before bind and
  between bind/run. Assert B remains valid, exact typed refusal, no installation
  and no spend. Test target retirement separately so the oracle cannot conflate
  the owners. Add gate-only fencing as well as canonical lease/epoch revocation.
- Enter generic ordinary and preview installers through their public execution
  paths. Assert typed unqualified/source refusal before other admission errors,
  unchanged public state and unconsumed preview. Pair with a valid service-issued
  install and a valid explicit bootstrap recipe.
- Barrier-control two binders of cached renderer code: exactly one Ready; loser
  has typed refusal. Independently admitted C reuses the immutable renderer and
  succeeds. Bound and spent admissions cannot be resealed. Test a real public run
  failure after the spend; private manual-spend tests are supplementary.
- Exercise cancellation/lost waiter before import, after native production,
  during final source admission, after native promotion and before final slot
  acceptance. Track actual roots/scopes/receiver and late producer settlement.
  Preserve an unrelated healthy installation and a borrowed same-session root.
- Refuse physical discard through the wrong owner, recover the SAME custody, then
  release through its owner and observe a real receipt. Unknown/unwinding cleanup
  retains uncertainty. Retain both the primary failure and secondary cleanup/END
  cause; missing state or a stopped process cannot stand in for release evidence.

Compile directly affected runtime, actor and host production/test consumers as the
shared boundary lands. Run counted owning controls and the actor notebook cohort
on the integrated new route. Retain exact source/artifact, selected/executed counts,
action/END/cleanup observations, exit status and logs. A two-thread test without a
barrier, linking a test target or an executed legacy path does not close the
corresponding requirement.

## Wave 2: retirement and recovery

### One retained attempt in existing owners

Extend existing RunState with one affine retirement attempt retaining expected A,
frozen candidate B, its owner-issued logical compiler-world/restoration product,
the real A-generation fence, exact coordinator identity and
references to existing stop/cleanup/publication observations. The attempt survives
loss of its initiating future. Necessary durable facts use existing operation and
recovery journals; there is no second selected-generation log, generic saga,
reload-only resource registry or detached teardown supervisor.

Prepare and validate the complete candidate, typecheck and provider-supported
surface before retirement. Invalid, unchanged and surface-refused candidates stop
nobody. At the capture transition recheck exact selected A and prepared B's
identity. B remains frozen if editable files subsequently become draft C; publish
and recover B without recapturing C. Obtain B's compiler world and restoration
roots from the SAME frozen candidate's source/profile preparation through the
existing world owner; the attempt retains that product, not a second selected
generation registry or a world rebuilt later from current files.

Under the existing admission and RunState owners, fence new A-dependent work and
capture active generation-dependent non-LLM work and registrations through their
existing owning classification. Cover reservations, admitted compiler work,
preparation, pending startup, serving resources, ownership transfer, callbacks,
forms and display work. Retain the native/value/source custody needed by those
owners separately. Inert historical code, values, source snapshots and settled
receipts are not blockers merely because they remain rooted. A receiver blocks
when its service/registration or admitted work requires settlement, not simply
because its code is retained. Source-ordered enqueue must
prevent a pre-fence reservation from being missed; late completion either belongs
to the captured attempt or refuses with its actual cleanup retained. New post-fence
work refuses before allocation. Do not hold a machine checkout or mutable actor
borrow while awaiting the directory, compiler END, shutdown or store IO.

Inspect physical lifetime edges as well as source dependence. If draining dependent
P would also kill independent child C, refuse before any stop; do not implicitly
reparent C. This collateral closure concerns proposed stops, not every retained
custody edge. Independent LLM worlds and independently owned/source-neutral
resources remain outside the stop set. Unobservable or
unclassified source-dependent resources are explicit blockers/uncertainty, not an
empty inventory. Default mode reports typed exact blockers, reopens still-selected
A through the owning attempt and stops nobody. Known live pending services can
continue after that unmutated refusal without waiting for them to terminate.

Force is independent of surface consent and needs no earlier preview receipt. It
authorizes only the actual captured set. Drain through existing lifecycle,
InvocationWork, request/form and physical cleanup owners; do not close the enclosing
LLM actor lifecycle or shared compiler preparation producer. Await accepted A
compiler work through its real END owner. Exclude only the exact already compiled
self-reload continuation and receipt delivery from terminal drain, never all work
with its actor/generation. Its own preparation ticket must already be settled.

### Settlement and reopening

| Observed state | Required transition and receipt |
|---|---|
| Default blockers or cancellation before any stop/publication effect | Consume the unmutated fence by reopening exact still-selected A; preserve bindings/services and report blockers/cancellation. Known pending startup is not unknown cleanup. |
| X stopped, Y has a known settled refusal; B known uncommitted | After every initiated stop settles, reopen exact A if the same attempt still owns its fence. Report X stopped and Y still live/refused; restart nothing. |
| Stop dispatched but acknowledgement/physical cleanup unknown | Retain the attempt, fence and actual obligations. Cancellation or future/PID disappearance cannot establish no stop effect. Require explicit settlement/recovery. |
| Drain confirmed, clean publication refusal | Reopen A only after exact-selection/fence comparison and known non-publication; retain every actual stopped-resource outcome. |
| Store commit or installation acknowledgement uncertain | Use existing publication-unconfirmed/unavailable recovery state. Do not infer A or B from a missing waiter, reopen automatically or replay reload. |
| B commit/installation confirmed, caller cancelled or continuation failed | B and its settled reload receipt remain authoritative. Retain the independent continuation failure and old physical custody; no rollback. |

Reopening requires all of: this exact attempt still owns the fence; A remains
selected; publication is known uncommitted; every initiated stop/cleanup outcome is
settled; and no retained uncertainty remains. A stop that failed with a known
settled outcome differs from a stop still queued or missing acknowledgement.
Record actual stop effects through existing operation owners even if publication
does not happen; a partial stop is not a non-destructive Rejected operation.

Only confirmed required drain permits coherent B publication of source, installed
tools, prompts, fresh notebook context and its matching logical compiler world
and restoration roots through the existing atomic generation owner. Retire A's
fresh admission while preserving actual admitted code/value
custody. Its exact compiled continuation may run pure code, inspect existing
receipts and perform independently authorized source-neutral work. New A compiler
tickets, installation, callbacks and registrations refuse at actual owners. Simple
precompiled receipt display is distinct from new renderer specialization. Do not
create a global effect-classification framework for that distinction.

### Recovery and endpoint acceptance

Extend existing reload result/schema owners with typed exact blockers, actual
stopped/refused/unconfirmed outcomes, selected-generation evidence and fence state.
Generate Rust/MCP/Haskell consumers from the schema owner. Keep force default false
and test all four force/surface-consent combinations through the installed authored
endpoint. Presentation must preserve stop effects and primary/secondary causes;
rendered labels cannot drive state transitions.

Required histories use real source capture/editing, installed endpoints, resource
admission, shutdown and durable recovery:

- Invalid candidate/no-op/default refusal: zero stops, A public state and live
  services preserved, independent LLM actor still serves. Force without earlier
  preview: exact affected identities actually stop, cleanup confirms, B is usable.
- Barrier-controlled preparation/startup/registration/transfer against capture:
  each resource is included or refuses with retained cleanup, never absent from
  both. Include same-label/different-incarnation and collateral-child controls.
- Cancellation before stop, after stop dispatch/before acknowledgement, after
  compiler response/before END, after END/before commit and after commit/before
  acknowledgement. Observe actual gates, stop multiplicity, selected generation
  and receipts, not just a returned reload constructor.
- X-stopped/Y-refused permits truthful settled partial reopening; X-stopped/Y-unknown
  remains fenced. Caller loss, late stop completion and recovery preserve those
  distinctions and never restart a stopped service or repeat a stop for an ACK.
- Self-reload completes receipt delivery and old pure/neutral continuation without
  waiting for its own terminal invocation. Stale A aliases, queued routes/forms,
  reply definitions and callbacks cannot create new work. Fresh B calls succeed.
- Cold recovery selects published B while draft C is present, restores exact
  source/prompts/installer, logical compiler world/restoration roots and the reset
  notebook contract, and does not replay
  initializer/cell effects. Corrupt selected artifacts refuse. Partial/uncertain
  retirement reconciles exact existing outcomes without reviving stale handles.
- A genuine demanded historical native dependency remains retained during drain
  and independent-child use. Retained custody does not become optional B demand;
  a real conflicting required dependency still refuses.

Use the existing proptest/hosted infrastructure for an independent shrinking
history model of exact owners, captured closure, fence, selection and actual stop
effects. The oracle must not call production capture/fence logic. Pair model
coverage with real endpoint histories, including the existing coherent-reload and
embedded-recovery tests; migrate their valuable assertions onto the new route.
The actor notebook cohort and affected runtime/actor/host/generated/Haskell targets
must compile and execute nonzero selections. A synthetic aggregate-Unconfirmed
latch does not qualify a real compiler post-response END-loss history.

## Integration, gates and completion

Wave 1 hands off shared admission/output constructors to runtime-publication,
then qualifies one early joined new-service history before expanding fixture
migration. Remove displaced finite/raw APIs, source-reconstructing adapters,
authority factories and stale callers in the same integrated cutover. Preserve
actual native provenance, historical custody and cleanup controls. Review final
joined consumers for alternate routes and source metadata lost at presentation.

For early Persona, source revocation, cancellation/uncertain END, stale publication,
truthful cleanup and non-destructive default reload refusal must work through the
new sole route. An unfinished force endpoint must explicitly refuse. This permits
the campaign's earlier Persona delivery only after all its ordinary/query/bundle
requirements pass; it is not completion of this lifecycle area.

Final acceptance requires full force, partial/uncertain retirement, forks, cold
recovery and physical fault histories on the exact integrated frozen bundle.
Retain base/head, dependency commits, actual bundle/artifact hashes, commands,
selected/executed counts, compiler END and cleanup receipts, affected identities,
selected durable roots before/after and logs. Report implementation, executed
acceptance, frozen qualification and deployment separately. Follow admitted pinned
build/test entrypoints and actual resource headroom; this plan runs no builds.

## Decisions to settle at the owning boundary

- Agree the exact consuming new-service output/admission signature and placement
  of original A with service-cells/runtime-publication before editing shared
  files. The retained-source invariant and deletion of finite fallback are fixed.
- Choose private RunState attempt representation and the existing operation/
  recovery record fields needed to survive caller/host loss. Demonstrate capture
  ordering and recovery with a small real history before broadening the API.
- Choose typed public blocker/partial/uncertainty constructors and an explicit
  wire/durable version decision where meanings change. Unsupported prior formats
  refuse deliberately; do not retain legacy execution to load them.
- Establish which existing physical owners acknowledge each captured resource
  kind and how the exact self-continuation is identified. An unsupported owner is
  a blocker until its real cleanup contract is supplied, not an independence guess.

These choices concern representation and integration, not permission to weaken
locked lifecycle semantics. Escalate a concrete counterexample if existing owners
cannot express a required transition; retain its evidence and continue independent
work rather than introduce a fallback or another tracker.
