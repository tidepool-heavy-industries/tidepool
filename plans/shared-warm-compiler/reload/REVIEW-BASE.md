# Reload authority review at the campaign base

Reviewer: `/root/warm_compiler_campaign/reload/authority_review`.

Current exact review: original campaign base
`5b74c2f1fe010c5c343e0c6f331d54328fbd5090`, previously reviewed owned history,
then core base `345194431d9f8407a7b7e7c4c273eafbce90bfc0` through committed
core candidate `51900a30df171a3920584eeaa12be0180d6c7bb4`, plus exact
fixture-only followup `a82c900783e13ee5bf8c107df7ab00ab737437ea`.
Disposition: CHANGES REQUESTED for the C6 core. Confirmed original/target lease
custody and Pending-to-Ready provenance gaps, plus the actual metadata-cycle
terminal-owner limitation, are recorded in the final section. Candidate owning
compilation and test execution remain 0; earlier foundation results do not
qualify this candidate. Parent explicitly authorizes integration for EARLY
COMPILER FEEDBACK with review-blocked status, not an acceptance fold or feature
acceptance. Source is frozen pending that compile receipt before further repair
growth. No reviewer native command or product edit was performed.

Required R1 WatchRoute/forms/reply-tool/display registration successors remain
explicitly HELD until initial runtime and actor production/unit compilation.
Their absent gates are known incomplete R1 coverage, not an accepted exception.
R2 force-drain/publication composition and R3 remain held. The following base
map supplies the issuance relationships and historical counterexamples; its
baseline line numbers are not current candidate locations. Current candidate
findings and accepted strategy are recorded at the end.

## Authority issuance and use

| Owner and source | Issued fact / next production use | Retirement obligation |
| --- | --- | --- |
| `resident_workbench.rs`, `ActorGeneration` (1714), `ActorRequestSnapshot` (1945), `InstalledToolsState::snapshot_for_call` (2060) | Complete selected installation/source/context/public owner/scope; endpoint selection retains its snapshot | Retained snapshot is immutable custody. New calls from it need the live generation admission owner, even if the selection predates the fence. |
| `InstalledToolsState::replace_explicit` (2131) | Reconstructs an `Arc<ActorGeneration>` with the same id/context/public owner/scope and changed installation | Preserve the same admission dependency. Pointer identity of the wrapper cannot define a new retirement domain or reopen admission. |
| `mount.rs`, `ActorSessionContext` (943), `with_issued_source` (962), `compile_view_with_inputs` (1007) | Pairs exact source roots with actor/placement/imports; compile view validates session/scope | Source equality and live scope establish neither currentness nor independence. Context needs owner-issued dependency that survives captures and re-entry. |
| `resident_actor/owned_workbench.rs`, `WorkbenchCompilationAuthority` admission near 40 | Validates source issuer and installation, retains immutable source and digest | Admission enrichment must carry the live dependency through the compilation owner; successful immutable validation cannot substitute for a fresh gate check. |
| `capabilities.rs`, `CompilerOwnerScope::admit_work` (93); `termination.rs`, `CompilerWorkTicket::admit` (323) | Registers Pending with actual lifecycle/invocation/preparation recipients before returning affine native obligation | Fresh dependent compiler admission refuses after the fence. An admitted ticket still settles its original END; shared observer cannot cancel an independent producer. |
| `resident_workbench.rs`, `with_compiler_machine` (5663), split compile/install callers | Captures task-local compiler owner, checks out machine, then opens a compiler ticket | A ticket currently covers continuation-only operations too. Separate retained re-entry scope from actual fresh compiler/install authority; blanket refusal here breaks self-reload. |
| `runtime/session/resident.rs`, `PendingPreparedInstall::compile_off_checkout` (2725), `install_admitted_turn` (7049), activation preview (6800 onward); `persistent.rs`, `install_certified_turn_in` (876) | Compiles or installs exact admitted input/certified native closure in a live lexical scope | Recheck retirement at actual new authority issuance and final installation. Scope liveness, matching snapshot, warm images and a clean END alone do not establish generation admission. Preserve genuinely pre-fence admitted work and independent worlds. |
| `start.rs`, `ResidentActorStart::capture` (160), `capture_spawn` (215), `capture_decoded` (325) | Captures exact entry, facades, placement and source-dependent child behavior | Typed services inherit the issuing dependency. Actual captured/fresh LLM worlds receive independent-world evidence at their owner; absence of snapshot/provider is not evidence. |
| `local_actor.rs`, `spawn_resource` (376), `spawn_worker_scoped` (661); `kernel/lifecycle.rs`, `resource_admission` (801) | Existing actor-incarnation admission and pending producer custody before native startup | Add generation dependency at the real reservation owner, before side effects, then include pending reservations in the fence. Closing the LLM actor lifecycle is too broad. |
| `local_actor/run_control.rs`, `RootPreparationRecord` (346), `RegisterResource` (1306), transfer reservations (2423 onward) | Directory owns preparation phase, placement, cleanup and resource/worker ownership | Extend these records, not a second inventory. Capture reservations and transfer state atomically with fencing. A resource finishing startup must join the captured set or refuse with retained actual cleanup. |

Fresh/captured LLM worlds cannot be recognized by placement. `capture_spawn`
retains a `SpawnDefinition`; `capture_decoded` produces a typed record service.
Fresh LLM startup may fall back to the parent's machine
(`resident_actor/child_launch.rs`, 497); captured LLM startup normally uses the
same machine with an isolated scope. Typed services may have a new machine.
Thus SessionId, source bytes, ancestry, labels, model grants and current provider
attachment are unsuitable substitutes for the actual world issuer. Preserve
explicit dependency through source-layer admission, remint, fallback and child
provisioning at `child_launch.rs` 464–537.

## Concrete alias and continuation counterexamples

1. Retain A policy; publish B; invoke a new Haskell cell through A.
   `coherent_reload_acceptance_tests.rs` 513 and 874 currently assert success.
   Migrate both late-new-call assertions to typed retirement refusal, retaining
   old prompt/source observation and exact B-selection assertions.
2. Attach A source callback; replace its recipient; fence A; deliver event.
   `request/sources.rs`, `CallbackExecution::transferred` (39), changes its
   snapshot to `SourceCallback`, retaining A source and old lexical scope.
   `ActorRequestSnapshot::generation` (1965) returns None for that variant.
   A gate that treats None as Independent admits the callback.
3. Start a source-bearing actor through context/startup; install a source;
   fence A; deliver event. `CallbackExecution::from_context` (29) has no
   snapshot/scope. `capture_startup_step` (10718) uses it, while boot Entry
   (`resident_actor.rs`, 9352) overwrites snapshot with `actor_program_snapshot`,
   which can be absent. Missing optional metadata cannot authorize independence.
4. Register A watch route; transfer recipient; fence A; mark watch ready.
   `WatchRoute` carries only recipient/root/state (`request/routes.rs`, 18).
   `transfer_owner` changes recipient and may requeue. `take_route` (77)
   returns a bare root. `resident_actor.rs::route` (13172) creates a compiler
   scope from the receiving actor context. An optional-snapshot-only gate misses
   this route or relabels A code under the receiving world.
5. Self-reload publishes B, then tries to return its receipt and evaluate pure
   old code. `source_effect.rs::resume_source_answer` calls `runner.resume_value`
   (11415), which calls `with_compiler_machine`, issuing a fresh ticket before
   `resume_classified`. `resume_unit` and `capture_boundary_mode` do the same.
   Rejecting every such A ticket prevents receipt delivery. Exempting the whole
   invocation instead permits later A compilation/registration. Actual runtime
   re-entry (`resident.rs::reenter_prepared`, 8579) structurally builds answers
   and runs already installed code; it can complete without fresh compilation.
6. Reserve A startup; fence while native startup awaits; complete registration.
   `RootPreparationRecord` currently lacks dependency/parent provenance and
   `RegisterResource` checks only parent existence and a pending preparation.
   Capturing completed directory entries alone misses this accepted producer.

## Resources outside the native directory

These have real existing owners and can be extended with private,
generation-filtered inspection and retirement operations. No new registry or
generic teardown scheduler is needed. Existing actor-wide cleanup is too broad.

| Resource | Existing authoritative membership and identity | Existing stop and observation; necessary extension |
| --- | --- | --- |
| Human forms | `InvocationWorkState.forms` (`invocation_work.rs`, 50/1001), `FormCleanup { actor, mount, host, result }` (`forms.rs`, 52); native `MountedForm` is a frame-owned lease | `FormCleanup::close` invokes durable `FormHost::close`, retries failed closes, and retains result. `InvocationWork::cleanup` (1126) aggregates failure. Inspect/close exact dependent forms here without closing the initiating invocation. `form_registry` contains weak observations and is not the complete inventory. Existing Await/Commit/Close settlement differs from opening a fresh typed form. |
| Watch routes | `RequestRegistry` owns each WatchRecord.route; WatchId, owner incarnation and `WatchRoute` retain waiting/queued root | `WatchRoute::retire` (68) removes queued entry and records Failed. Actor-wide `actor_stopped` (3101) calls it. `forget_watch` refuses active routes; transient-watch cleanup does not stop persistent routes. Add dependent-route inspection/retirement under this registry. `take_route` moves a running root into the route's `InvocationWork`; retain that actual running settlement separately before claiming release. |
| Attached sources | `ResidentKernelBehavior.sources` stores exact SourceBinding roots/execution; `ActorSourceConnections` owns request/lifecycle/command subscriptions | `close_sources` (12141) drops connections only. Actor stop (13646) also clears roots. Add exact dependent-source removal and subscription detachment at these owners. The existing request connection guard owns a set, so selective detachment needs an owner-level operation, not filtering a copy of the source list. Account for deliveries already queued and a mapper already running. |
| Expandable/selectable displays | `ActorDisplays.slots` and `.pending` retain callback/execution; identity is actor incarnation + slot + publication ordinal | `ActorDisplays::retire` (757) clears callback/execution while preserving unsettled emission metadata; `select_mode` (956) marks an expansion in flight. Add generation-filtered inspection/retirement in this owner. Active expansion holds cloned callback outside the slot; clearing the slot is not proof of its release. Keep pending/unconfirmed DisplayPublication outcomes. Nonselectable pages without expansion keys already drop callback/execution at `stage` (851), so simple receipt text need not become a blocker. |
| Reply-tool definitions | `RequestRegistry::register_reply_tool` (1718) retains ReplyToolDefinition on exact reserved request; `resident_actor.rs` (7663) prepares before registering; recipient activation uses installed reply slot | New source-dependent definition preparation, registration, transfer and activation need the dependency. Existing reply/result/cancellation settlement stays under its original request authority. A generation gate must not classify every reply operation as a new service. |

Native `spawn_resource`'s sole production caller is command creation
(`command_jobs.rs`, 600). Its existing `CommandBackendPurpose` distinguishes
normal commands and SourceProbe (`start_with_purpose`, 558). A normal independently
granted process operation may be source-neutral; source probes have a source
dependency at their issuing handler. A blanket "all resources depend on the
current notebook" rule would break neutral continuation semantics.

This map exposes no new hard architecture question: the owners can gain narrow
dependency-aware membership/retirement methods. It does expose substantial
unfinished implementation. Register/activate/fence ordering and in-flight
settlement must be joined; merely rejecting future entry leaves old registrations
uncounted and cannot justify default/no-blocker publication or force cleanup.

## Smallest independent operation-history oracle

Use small logical identities independent of production handles:
`World(actor_incarnation, generation, issuance_nonce)`, `OperationId`, and
`RegistrationId`. Equal source/interface bytes may belong to different worlds.
Keep a plain list of issued facts: current selected world, gate state, admitted
operations, reservations, registrations, and observed cleanup/publication facts.
Do not derive expected dependency from production snapshots, generation(), cache
keys, source digests or registry output.

Generate IssueAlias / AdmitCompile / ReserveRegistration / FinishRegistration /
Fence / Transfer / Activate / Complete / Drop operations. Add explicit
IndependentFork and confirmed/unknown stop outcomes. Before each transition,
save the model facts; replay using actual production constructors and consumers,
then compare only observed outcome and membership against the saved model.
An alias/transfer changes handle ownership, never its dependency. A fresh A
admission after its fence refuses. A reservation accepted before the fence is
captured even when native completion follows it. Existing admitted operations
settle original receipts. A private prepared B is not selected until publication.

Targeted histories embedded in arbitrary prefixes/suffixes:

- Old policy and SourceCallback aliases; from-context startup and WatchRoute
  transfer; both orders of registration versus fence.
- A and independent fork F share exact source bytes; fence/retire A; F still
  compiles, uses its exact historical native child and eventually releases it.
- Same actor label/id with distinct incarnation; equal generation numbers with
  distinct world issuers; neither can reopen the other's fence.
- Default blockers release an unmutated fence; force stops X, Y refuses with
  known outcome and A reopens; X stops, Y is unconfirmed and A stays fenced.
  Missing/inaccessible owner cannot become Confirmed or an empty blocker set.
- Self-reload Continuing returns receipt, computes an old closure, reads text
  using an existing neutral grant, and then tries fresh compile/install/source
  callback/service registration. The first three progress; new dependent work
  refuses; B remains selected even if the continuation errors/cancels afterward.

Keep real hosted tests for GHC provenance, display/form behavior, independent
LLM capture and durable selection; a small model cannot prove these. Build the
minimal driver only after private owner APIs exist: no test-only grant factory.
Use deterministic barriers at admission, registration and owner settlement;
sleep and an unpolled future do not establish the race. Calibrate with isolated
mutations that discard transferred dependency or omit pending reservations.

## Pending verification

Observed `/srv/build` free space: 102 GiB, with the mandated 100 GiB unreserved.
No cold 32 GiB build is admitted; no admission bypass, cache deletion, daemon
restart or live runtime change was attempted. Source review is not a test pass.
Once admitted capacity and implementation exist, the owning build commands are:

```sh
swarm-build bash scripts/buck2-configure.sh --tests --browser
swarm-build just build //exomonad/actor:exomonad_actor_unit_tests \
  //exomonad/host:exomonad_host //exomonad/host:exomonad_host_unit_tests
swarm-build just test-native //exomonad/actor:actor_notebook_contract_tests
swarm-build just test-lib exomonad-host \
  --exact actor_host::coherent_reload_acceptance_tests::coherent_reload_replaces_the_notebook_and_prompts_without_rewriting_issued_work \
  --expected-count 1
swarm-build just test-lib exomonad-host \
  --exact actor_host::coherent_reload_acceptance_tests::authored_sync_reload_publishes_an_opted_in_surface_and_waits_for_settlement \
  --expected-count 1
```

These commands are pending, not executed. New oracle and failure-case selectors
must be recorded from their actual owning test definitions. A final independent
review must inspect the complete exact `base..candidate` delta, affected test
compilation, nonzero execution evidence and one joined production history.

## In-progress implementation review and bounded strategy findings

The subsequent read-only review inspected uncommitted work in
`/srv/swarm/checkouts/tidepool-shared-warm-reload-admission-20261019`, still based
on exact `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`, and the area's uncommitted
directory seam. This is steering for work in progress, not an exact-candidate
acceptance receipt. No test ran. Concrete findings were sent directly to the
generation-admission owner and area supervisor.

- The partial diff preserved dependency through SourceCallback transfer and
  explicit installation replacement. Its generic descriptor constructor still
  issued IndependentWorld, and a missing compiler dependency still fell back to
  continuation admission. These fail-open defaults require explicit owner
  qualification before acceptance.
- A momentary `with_open_admission(|| ())` before capture or transfer does not
  atomically reserve the subsequent resource. The directory's proposed gated
  enqueue followed by FIFO population observation provides the right ordering
  seam once all production reservation/transfer callers use it.
- The directory population must observe abandoned and forgotten reservations
  through retained lifecycle/terminal evidence. No entry is not confirmed
  cleanup. `LifecycleHandle::run_finalized` is authoritative but only reachable
  through whole-run finalization. For never-registered actors, the existing
  lifecycle finalizer's predicates distinguish known absence of an executor
  from pending/lost cleanup: no executor/resident, closing state, no pending
  transactions/tasks/native work/transfer uncertainty, and an available owner.
  Expose that distinction from its lifecycle owner rather than reconstruct it
  in the population scanner.
- Skipping a native ResourceOwner::Retired has an actual current production
  proof: the sole `forget_resource` caller in `local_actor.rs` handles a linked
  child terminal event, awaits `resource.cleanup(Observe)`, and forgets only on
  Confirmed. `command_jobs.rs::Shared::resource_cleanup` returns Confirmed only
  for retained CommandClean; backend unknown/retained stays unconfirmed. The
  other Retired assignment is conditional on RunResourcesSettled Confirmed.
  Preserve this owner-derived contract if the forget API acquires more callers.
- The follow-up lifecycle projection `unregistered_cleanup_confirmed` shares
  native-settlement predicates with FinishRun, but is not by itself a monotone
  terminal receipt. `AttachCleanupExecutor` explicitly permits Closing with no
  executor, and LocalActor's startup-refusal paths can attach that cleanup
  executor later. Population omission must additionally prove the actual
  producer cannot subsequently publish physical cleanup custody, or retain its
  corresponding pending preparation. A controlled close-before-cleanup-prestart
  history must prohibit publication until actual settlement.

The campaign approved placing the source-admission fence with the runtime
lexical owner. This avoids an actor dependency in runtime and allows retained
physical custody independently of fresh compilation/installation authority.
Adversarial requirements for that design:

- `retain_lexical_scope` creates a new detached scope and lease, rather than
  returning the source scope's canonical lease. `PrivateExecutionAdmission`
  also directly constructs a lease. Propagate the same prerequisite gates
  through these paths, `mint_scope`, `mint_detached_scope`, declaration
  successor scopes, `mint_scope_from_lease`, and installation scopes.
- A typed dependent child needs the parent prerequisite plus its own gate.
  Its reload cannot sever the parent's source dependency. An actual independent
  LLM world detaches at its explicit capture/admission owner, retaining exact
  historical bytes and dependencies. Different scope/session alone is no proof.
- SourceCallback transfer changes recipient context placement while retaining
  the old scope in `CallbackExecution.scope`. `map_source` runs using its
  execution context. Checking only the current runtime lexical scope can see
  successor B while executing A code. Preserve the original source prerequisite
  at the operation's actual compiler/installation/registration claim.
- `resume_classified` can finish a Plain/Value hole without compiler work, but
  Binding/Projected completion calls `bind_prepared`, which can reach declaration
  projection/interface compilation through `bind_replacing_decls_in`. A
  continuation-only scope must not grant that hidden fresh compilation.
  Conversely do not fence immutable source/status/receipt observation or
  lexical-lifetime validation; those facts remain usable under custody.
- A Ready activation renderer can install without a new producer ticket.
  Recheck actual install admission, not only the renderer Produce branch.
  Fresh compiler claims retain their actual END lifetime; pure continuation
  custody must not be counted as work that reload waits on itself to finish.

### Independent world versus actor-owned cleanup

The following counterexample is reachable through production constructors:

1. Root A starts typed dependent service P with AgentLaunch, SpawnAgent and a
   delegated model grant. `role.rs::admit_record_child` explicitly permits model
   grants without a conversation; nothing forbids P from a later fresh spawn.
2. P calls `spawnSubagent FreshCtx ... defaultSpawnOptions`. The Haskell default
   lifetime is ActorOwned (`Tidepool/Actors/Spawn.hs`, 180), and `capture_spawn`
   assigns P as supervisor for a non-RunOwned child. C owns an independent source
   world but still has P's explicit cleanup lifetime.
3. Force stops P using ordinary `finish_actor -> shutdown_children`.
   That cleanup stops all actor-owned children, including independent C. Merely
   excluding C from the generation population does not preserve it.

There is an existing explicit lifetime transfer: P can invoke
`retain_agent_resource(C, RunOwned)`. `reserve_worker_lifetime_transfer` holds
P's resource admission, and CommitWorkerTransfer plus WorkerRetentionPublication
unlink C, update its scheduler/descriptor and remove invocation membership
atomically. This is owner-authorized transfer, not a new lifetime mechanism.
Root A cannot invoke it as itself, and silently promoting C would change its
explicit ActorOwned lifetime.

The minimal contract-preserving retirement option is to inspect each affected
P's cleanup closure before stopping it. If preserving an independently owned
world cannot be proved, report a typed cleanup/refusal blocker for P and stop
nobody through that unsafe path. Force does not certify an unsupported cleanup;
the existing partial/refused outcome contract can represent the refusal. An
explicit retention transfer can remove the blocker before a later reload.
Automatic promotion or reparenting is a separate lifetime-authority decision
for the campaign, not an implementation default. Cover this history through
the real typed-service spawn, LLM child, transfer and shutdown owners.

Awaited event: an explicit candidate/checkpoint from the generation-admission
owner or area supervisor. Next reviewer action: inspect the complete supplied
base-to-candidate delta and retained compilation/execution evidence, including
the source-prerequisite and cleanup-lifetime cases above. No periodic polling
or background wake loop is required.

## Exact directory checkpoint review: dc2c858795

Reviewed the complete diff from
`5b74c2f1fe010c5c343e0c6f331d54328fbd5090` to
`dc2c85879579f5fa3543f4bde3d2c3fa2dce3b2b`, containing only directory reservation
metadata and the new generation population/enqueue module. Source-only review;
`git diff --check BASE CANDIDATE` passed. No compilation or execution. The
checkpoint depends on the pending sibling gate API and contains no reload
composition or force drain. It is not integration-ready.

**Blocking finding:** NotStarted conflates refusal and loss. At this candidate,
`ReservationReady { lifecycle: Err(_) }` always releases/restores the identity
claim and marks NotStarted (`run_control.rs`, 1786–1796); population skips that
phase. However `preparation.rs::VerifyTerminal`, WorkerKind::Reserve (578),
sends the same Err channel when a reservation starter dies without settlement.
That death may follow lifecycle allocation and precede result publication.
The allocated lifecycle is linked to the preparation supervisor rather than the
dead starter, so worker death does not prove its cleanup. The needed repair is
an owner-issued typed distinction between known pre-start refusal and unknown
starter/lifecycle settlement; unknown retains identity and cleanup obligation.

The adjacent baseline failure category also affects whole-run cleanup. Releasing
the claim and marking Abandoned makes `preparation_pending` false;
`settle_preparation_waiters` can report Confirmed, and FinishRun's registered
population/finalization grants omit a record with no published lifecycle handle.
An explicit unconfirmed phase counted as pending prevents those false
confirmations. `terminal.request_shutdown` alone does not settle the lost
lifecycle. The supervisor's existing Reserve worker record retains only identity
and terminal, so missing handle provenance must remain unconfirmed until actual
owner reconciliation rather than be reconstructed from names.

**Joined invariant still pending:** a generation preparation may produce an
Actor from a separate legacy reservation whose dependency is None. Accepted or
Settled(Confirmed) preparation records are omitted by population without an
owner-issued product-reference check. Native resource registration correctly
derives dependency from its preparation record. Actor output needs equivalent
owner-enforced transfer into its actual reservation, or an exact retained output
reference in the existing preparation owner, before that record can be omitted.
Caller migration alone must not be the invariant.

The new gated enqueue followed by directory observation has the correct ordering
shape: enqueue occurs while admission is held, and the subsequent fence's
population message cannot overtake that synchronous send. Abandoned/no-entry
records now preserve uncertainty. `depends_on` is the necessary membership
relation for typed child prerequisites, but its actual issuing implementation
is outside this checkpoint and remains unreviewed here. Independent collateral
cleanup remains governed by the confirmed counterexample above.

Disposition: request repair of the typed starter failure distinction; preserve
the preparation-to-product invariant as a required joined check. Producer:
reload area supervisor. Awaited next event: an explicit exact repair commit and
its cumulative delta/evidence, followed by the integrated sibling gate candidate.

## Exact repaired checkpoint review: 96563ebbb9

Reviewed the complete source delta from
`5b74c2f1fe010c5c343e0c6f331d54328fbd5090` to
`96563ebbb95438e13faba7acf3ac2663d6ed4255`, including the directory foundation,
real worker preparation caller, starter/supervisor failure paths, and narrow
force-option schema/authored/codec foundation. No new blocking source defect
was found in this checkpoint. Both prior concrete findings are repaired in
source:

- `ReservationStartFailure` distinguishes NotStarted from Unconfirmed at the
  issuer. Supervisor worker loss, predecessor-retirement uncertainty and
  lifecycle-start failure issue Unconfirmed. That branch retains the identity
  claim and StartUnconfirmed remains `preparation_pending`; Seal and
  AbandonReservation preserve it. Only known refusal before lifecycle startup
  or failure to enqueue/start the starter issues NotStarted. Whole-run waiters
  and generation population consequently retain unknown startup cleanup.
- `prepare_worker_root` replaces the generic generation-labelled preparation.
  The actual `KernelContext::prepare_worker` consumer supplies its reservation;
  BeginPreparation derives dependency from that exact Ready, unused reservation
  and records single-use preparation membership. RootWorker first publishes
  produced custody to the existing supervisor, then verifies that successful
  output identifies that reserved actor before delivery. Mismatch refuses with
  real custody still held for the existing supervisor's abandoned cleanup.
  Failure/cancellation before delivery preserves the same owner path.

`reserve_generation_actor` preserves the distinction between RunOwned cleanup
and source dependency: it reserves a root without a supervisor parent while
recording the actual generation prerequisite. `depends_on` remains the required
transitive membership operation; the sibling issuer implementation is not in
this checkpoint. Independent LLM collateral shutdown must still receive the
previously specified refusal control when force composition is implemented.

The force field's authored default is false and its policy consent is
independent. The codec fixtures cover all four force/policy combinations,
malformed Boolean input and old constructor arity refusal. These are source
changes, not executed codec evidence. Generated mirrors and affected target
input mappings remain pending through their owning generators.

This verdict does not establish integration readiness or runtime behavior.
The sibling admission API is not folded; this checkpoint does not compile
standalone. There is no R2 fence/force drain/publication composition in the
candidate. R2/R3 remain on the campaign's hold until the actual issuing
production/test consumers compile and the focused gate executes.

### Deferred regression selections and necessary fault controls

After source registration/generation and an admitted joined compile, execute
these existing selectors through the counted runner (all expected count 1):

```sh
swarm-build just test-lib exomonad-actor \
  --exact local_actor::run_control::cluster_tests::reservation_issued_before_seal_cannot_authorize_later_reservation \
  --expected-count 1
swarm-build just test-lib exomonad-actor \
  --exact local_actor::run_control::finalization_tests::uncertain_preparation_refuses_finish_and_keeps_actual_boot_lease \
  --expected-count 1
swarm-build just test-lib exomonad-actor \
  --exact local_actor::tests::admitted_pre_start_is_included_in_sealed_retirement_snapshot \
  --expected-count 1
swarm-build just test-lib exomonad-actor \
  --exact local_actor::tests::lifecycle_close_during_startup_settles_prepared_behavior \
  --expected-count 1
swarm-build just test-lib exomonad-actor \
  --exact local_actor::tests::startup_ownership_precedes_live_link_and_survives_reparenting \
  --expected-count 1
swarm-build just test-lib tidepool-bridge-effects \
  --exact context_constructor_tests::reload_options_and_typed_receipts_roundtrip_and_reject_wrong_arity \
  --expected-count 1
swarm-build just test-target tidepool-mcp mcp \
  --exact generated_surface_contracts::reload_defaults_and_structured_receipts_execute \
  --expected-count 1
```

These existing cases do not cover the newly identified starter-loss and
generation-product invariants on their own. Extend the existing directory
cluster/finalization and startup tests, without a parallel framework, with
deterministic barriers and actual production reservation/worker issuers:

- Stop the ReservationStarter after `LifecycleHandle::start_linked` succeeds
  but before its ReservationReady result reaches the supervisor. Observe
  StartUnconfirmed, retained identity, nonempty generation population, and
  inability to certify whole-run finish. A separate known prestart refusal
  must produce NotStarted with no false cleanup blocker.
- Drop the reservation waiter, abandon and seal in both orders while starter
  loss is outstanding. Neither operation may erase Unconfirmed or turn a
  missing lifecycle handle into proof of nonallocation.
- Start worker preparation using a genuine generation reservation; pause
  before native pre-start, fence, and inspect population. The reservation and
  preparation retain the same issued dependency until the actual result/cleanup.
  Repeat for RunOwned to check source membership without supervised lifetime.
- Use a second genuinely issued actor as a deliberately mismatched producer
  result. Verify that the first preparation refuses delivery and the real
  supervisor retains/settles cleanup of the produced actor. Confirmed and
  unconfirmed cleanup controls must remain distinct; no fake identity/grant.
- Attempt a second preparation from the consumed reservation. It must refuse
  before running its producer and leave the original result usable.
- Join typed child prerequisite A plus child gate P and independently captured
  LLM C. Source membership is transitive; cleanup ownership remains separately
  observed. Force retirement must refuse collateral destruction of independent C.

All commands and new controls above are deferred, with zero newly executed
tests. The measured disk blocker remains 102 GiB available against a 100 GiB
unreserved requirement, insufficient for a cold 32 GiB build. No bypass or
cleanup of shared outputs is authorized by this review.

Awaited producer/event: reload supervisor supplies an explicit joined candidate
including the sibling gate, regenerated inputs/mirrors and actual compile plus
focused execution evidence. Next reviewer action is cumulative exact-candidate
review with the startup-loss, dependency propagation and force refusal controls.
Yield after this report; no periodic polling.

### Compiler-owner bridge: 804a4d3e2f

Additionally reviewed exact
`96563ebbb95438e13faba7acf3ac2663d6ed4255..804a4d3e2f253f69ef81ca76eacc6229cfe7deea`
and checked the cumulative `5b74c2f..804a4d3e2f` delta for whitespace errors
(passed). The narrow change carries the reservation-derived dependency from
BeginPreparation through `PreparationMessage::Start` to the actual RootWorker
compiler scope. Generation-dependent work receives `with_source_dependency`;
the explicit native/preparation branch receives `for_preparation`. No new
blocking source finding in this bridge.

This bridge depends on the sibling's unmerged compiler-scope methods, so its
types and actual claim lifetime remain unqualified. In the joined review,
verify every source-bearing production reservation selects the dependency path;
the generic preparation branch cannot certify an unknown source world. Also
verify that a generation fence rejects a fresh compiler claim from a previously
reserved worker while preserving that worker's actual pending cleanup and END
observations. The earlier source-only verdict and all deferred controls remain.

Current reviewed source tip: `804a4d3e2f253f69ef81ca76eacc6229cfe7deea`.
Producer for the next event remains the reload supervisor: an explicit joined
R1 candidate with generated source mappings/mirrors and actual compilation plus
focused execution evidence. No periodic wake or poll loop.


### R1 bootstrap-alias strategy review (source-only, 2026-10-19)

Reviewed the admission checkout's actual uncommitted consumers at HEAD
`dc2c85879579f5fa3543f4bde3d2c3fa2dce3b2b`, against campaign base
`5b74c2f1fe010c5c343e0c6f331d54328fbd5090`. This is an in-progress strategy
review, not exact-candidate acceptance. The area source review remains through
`804a4d3e2f253f69ef81ca76eacc6229cfe7deea`; subsequent report/document commits
do not establish a joined source candidate.

Confirmed counterexample: `resident_workbench/generation_admission.rs`
`SourceDependency::Independent` currently returns `Ok(None)` from
`runtime_admission` and executes `with_open_admission` without a fence. An
independent descriptor/context captured before its first A publication keeps
that exact world object after publication. `request/sources.rs`
`CallbackExecution::from_context`, transferred SourceCallback snapshots and
`activation_context` preserve the captured dependency, so checking current
InstalledTools does not close these aliases. `capabilities.rs`
`CompilerOwnerScope::source_claim` authorizes their compiler scope with no
source claim. `ResidentActorMachineAccess::with_machine_wait` resolves the
same optional admission before `ResidentSession::with_source_admission`;
fresh runtime mutation/install claims can consequently also see None.

Campaign accepted the bounded owner strategy: the existing IndependentWorld
Arc owns first-publication Open/Publishing/Unavailable state and one-time
binding to the first actually visible A gate. Begin Publishing at the actual
initial publication claim before commit IO can become uncertain; do not bind
on candidate preparation. Bind to A in actual visibility publication. Only
confirmed nonpublication/no effects can reopen the unbound world. Unknown
commit remains unavailable even if `InstalledToolsAvailability::Available`
still holds and on_visible did not run. Future B copies only external
`Option<GenerationDependency>` prerequisites, not its own bootstrap alias to A.
This prevents A retirement from accidentally closing B. No new registry or
lifetime authority is needed for that owner contract.

Remaining implementation-critical atomicity condition: observing Open and
returning None is insufficient. A concurrent publication cutoff may occur
between that observation and the compiler/runtime source action. The unbound
world must atomically admit and retain actual source-work custody, and first
publication must either settle prebinding work before binding or preserve the
same actual admission gate/claims in A. The current compiler scope
`runtime_admission()?.map(admit)` and runtime checkout's cached optional gate
are concrete consumers of this race. Rechecking at an earlier callback or
context boundary cannot repair it. Send this condition to the leaf before
accepting the publisher seam.

Runtime publication ordering was inspected directly:
`persistent.rs::commit_notebook_generation_with_completion` claims commit,
performs `staged.commit`, calls on_visible after committed graph publication,
then awaits `receipt.installed_blocking`. Both RecoveryRequired before
on_visible and failure afterward require closed old aliases; the latter has
already bound A. A proven StaleRevision rollback/rebuild refusal can settle
previsibility, but rendered error text must not decide reopening. Restored and
ephemeral publication paths need the same actual-world binding semantics.

Independent LLM capture mints a distinct world in `start.rs::capture_spawn`;
its first A must bind that child's aliases and must not bind its parent's
world. Typed `capture_decoded` inherits the parent's source dependency; child
P then needs parent external admission plus its own P gate. ActorOwned versus
RunOwned remains a separate cleanup contract. The startup decoder
`capture_startup_step` permits source/shutdown/attachment/readiness operations
and rejects arbitrary child-start effects, so that startup phase does not
supply a proven pre-parent-generation typed-child counterexample. Generic
workbench/bootstrap operation histories remain unqualified: if they permit
such children before parent A, extracting only generation() None would erase
the future parent dependency, requiring an owner-level refusal or retained
bootstrap prerequisite. Repeated `admit_independent_world` constructor sites
must preserve an already issued canonical world across handoffs; fresh world
issuance may not strand earlier legitimately qualified aliases.

Required deterministic production-history controls, extending existing tests:

- Capture descriptor context, compiler scope, from_context source binding and
  transferred callback before first A. Publish A, fence it, and attempt fresh
  compile, ready renderer install, completed binding continuation and resource
  registration through each alias. All source work refuses; pure continuation,
  readonly status/source observation and existing receipt settlement progress.
- Pause independent compiler admission between selecting the world and actual
  source action. Race initial publication cutoff. The action either belongs
  to a counted pre-cutoff obligation or refuses; no uncounted None path.
- Pause initial durable publication before actual IO and after visibility.
  Confirmed previsibility refusal reopens only the original unbound world;
  unknown commit before on_visible remains closed, and postvisibility install
  failure keeps aliases bound to unavailable A.
- Prepare then discard candidate A: this must not permanently bind old aliases
  to an unpublished gate. Publish first A exactly once, then B: old bootstrap
  aliases remain A-bound while B is open until its own/external gate closes.
- Fresh/captured LLM child C publishes its own A independently of parent A;
  typed child P retains parent plus own admission. Include a real pre-generation
  child attempt to establish refusal or correct bootstrap dependency retention.

Zero newly compiled targets or executed tests. The same measured disk blocker
and source-only unqualified status apply. Awaited event: admission leaf produces
an explicit joined candidate implementing the accepted bootstrap admission
owner and publisher seam; reload supervisor supplies that exact source plus
compile/focused execution evidence when resource admission permits. Next
review: inspect atomic claims, unknown-commit state, actual alias histories and
external prerequisite propagation cumulatively. No periodic polling.


### Initial-publication uncertainty custody and recovery strategy

Read-only bounded follow-up reviewed the admission checkout's uncommitted
publisher, runtime and world-owner changes at HEAD
`dc2c85879579f5fa3543f4bde3d2c3fa2dce3b2b` (campaign base `5b74c2f1`).
No joined candidate or native execution is established by this review.

Confirmed gap: the bootstrap world now retains the exact target dependency
when WorldPublication drops uncertain, but the previsibility publisher still
leaves InstalledTools Available. Its on_visible closure owns and drops the
candidate's installation/context/scope/prompts/prepared public owner when the
callback never runs. Subsequent preparation through that unavailable world
refuses. This is a real missing retained-owner handoff, not evidence that the
candidate became selected.

Do not repair this by constructing prospective ActorGeneration and marking it
GenerationUnavailable before actual publication. ActorGeneration carries a
WorkbenchPublicOwner, and `PreparedWorkbenchGenerationOwner::bind(None)`
creates an owner whose `is_current()` returns true because no native readiness
is present. For a durable candidate, only actual runtime readiness can complete
that owner. `prepare_generation` validates canonical identity and placement;
it does not issue durable readiness. C2's actual-selection distinction applies
even when a surrounding enum currently prevents use.

Minimal retained type within the existing installation owner:
`InitialPublicationUnconfirmed { previous_bootstrap, candidate, uncertainty }`.
Candidate is affine, immutable preparation custody: exact installation,
context, lexical scope lease and its original gate, prompts, configuration
digest, exact prospective ActiveNotebookGeneration references, and the UNBOUND
PreparedWorkbenchGenerationOwner. Split these facts from the consumed runtime
PreparedNotebookGeneration rather than cloning publication authority or
storing a fabricated bound public owner. Retain them before risky publication;
on_visible consumes those same facts with actual readiness, binds the same
world to the exact retained gate, and only then constructs ActorGeneration.
If no callback runs, keep the remaining candidate facts in the uncertain
installation state. Available/current_generation/snapshot and replacement
must refuse source authority in this state; status may expose the previous
bootstrap's tools as SourceUnavailable. The retained gate is never reminted
from an ID or broader inventory.

Known typed no-commit refusal restores the exact previous bootstrap and reopens
only its existing world owner, allowing a different candidate to be prepared.
Unknown commit cannot be converted to no commit by failure to observe selected
state. Actual selected readback must equal the entire retained selection,
including references, and native readiness must validate the exact retained
scope before consuming the prepared public owner. A different selected head,
missing/unreadable evidence or failed readiness leaves source unavailable.

Important limit: there is no existing same-run reconciliation consumer for a
real native RecoveryRequired result. `recovery_store/owner.rs` routes both
ReadGeneration and Check through Command::Request; Phase::Fenced replies with
the retained error to all such requests. Unknown commit and lost committed
installation acknowledgement enter this permanent phase. Calling
`active_notebook_generation` or `durable_public_readiness` again cannot recover
that store. `persistent.rs::active_notebook_generation` uses the same store;
`admission.rs::durable_public_readiness` validates the installed graph, public
scope mapping, declaration root, epoch and retained run owner. It cannot safely
manufacture missing graph installation after pre-on_visible uncertainty.

Existing recovery evidence uses reopen: the production actor startup selection
in `resident_actor.rs::select_initial_configuration` reads the actual selected
head through startup_generation, validates exact installation/source/prompt
references and restores that source. The runtime regression
`recovery_store::tests::notebook_generation_commit_and_crash_recovery_select_one_complete_generation`
explicitly drops the fenced store, opens it again, and reads actual selection.
This is recovery of the enclosing runtime/actor lifetime; it does not reopen
old captured aliases or bind a new same-ID gate into their world.

Campaign decision: use EXISTING explicit host/run reopen recovery. No live
native-store reconciliation is in scope. Unknown pre-on_visible publication
leaves the old world Unavailable and all its bootstrap aliases permanently
closed. Same-actor/same-store retry refuses. InitialPublicationUnconfirmed
retains private facts for truth and custody only, never recovery authority.
Explicit reopen selects the actual durable head, complete references and real
readiness into a NEW recovered context. It does not reuse or rebind the old
gate, construct a prospective ActorGeneration, or call bind(None) for a durable
candidate. This resolves the recovery strategy; same-actor reconciliation is
outside the bounded repair, not an unresolved blocker or promised behavior.

Required controls: known no-commit refusal followed by a different candidate;
unknown commit with no on_visible retaining the exact candidate and gate;
unknown commit that actually selected A followed by real reopened-owner
readback of A's complete references; post-on_visible install failure retaining
actual GenerationUnavailable; never bind(None) for durable candidates; and
assert all old uncertain context/callback/compiler aliases remain permanently
closed even after a new recovered context progresses. Recovery tests must not
swap store handles or clear a fence behind production owners.

Status: source-only strategy findings delivered to reload supervisor and
admission leaf; whitespace check passes, zero builds or tests executed. Native
R1 compilation remains unqualified pending root's post-cleanup admission
receipt. Recovery strategy is decided as above. Awaited event: admission leaf
provides the exact candidate implementing retained uncertainty custody and the
all-actor bootstrap wrapper below. Next review inspects candidate custody,
actual visibility/readiness binding and known-refusal controls. No periodic
polling.


### Typed-child bootstrap aliases: approved parent-plus-own repair

Confirmed against current admission WIP: `start.rs::capture_decoded` stores
SourceDependency::Generation(parentA) directly in the typed child's descriptor.
After child's first C is visible, a pre-C descriptor context, compiler scope or
from_context source callback still carries A alone. Fencing C while A remains
open therefore does not close those aliases. Snapshot callbacks that already
carry C do not repair the independent old-descriptor path.

Campaign approved one private BootstrapWorld for EVERY actor. The actual typed
capture issuer creates it with external prerequisite Some(parentA). Root and
independently captured/forked LLM issuers create external None. This is the same
existing source-admission world owner generalized to typed actors, not a new
registry or cleanup/lifetime authority. Preserve its exact Arc through
Descriptor/session_context handoffs, initial tool lease and startup callbacks;
constructor convenience paths must not remint an already admitted wrapper.

Before C binds, actual source claims retain BOTH the child's own bootstrap
obligation and external A's runtime claim. CompilerOwnerScope::source_claim,
checkout_admission, registration_dependency and with_open_admission must all
observe the same state. Unconfirmed native close preserves both obligations.
The C publication owner bypasses only its own bootstrap checkout claim so it
can establish the Publishing cutoff without self-deadlock; it still needs an
actual external A claim through commit and installation settlement. A's earlier
precheck alone cannot authorize C publication after an A fence.

On actual first visibility, bind the wrapper once to C, whose runtime gate
already depends on A. Every pre-C descriptor/context/compiler/callback alias
then resolves C forever. Creating replacement D copies only external A; it
must never inherit its own bootstrap wrapper's bound C. New D aliases use D,
old bootstrap aliases remain C-bound. Child and parent identities, optional
generation metadata and runtime SessionId are not substitutes for this owner
chain. Independently captured LLM child E remains external None regardless of
ActorOwned/RunOwned cleanup ownership; the earlier independent-collateral
refusal contract still applies.

Pending pre-C worker registration may project external A for the existing
directory, while retaining its own bootstrap claim until actual enqueue and
allowing the directory's A gate to recheck atomically. A root/independent
bootstrap has external None but still retains the same own-world registration
obligation. No bootstrap-to-None projection may silently discard an external
prerequisite or prematurely drop its own claim.

Add deterministic histories through the existing actual capture/publisher and
callback/compiler consumers: parent A open, capture typed P and all pre-C
aliases, publish C, fence C, then fresh work through every old alias refuses
while A remains open. Independently fence A before C publication and verify
C refuses without publication; race A fence against an admitted C commit and
observe the real claim through settlement. Publish C then D, retire C, and
check D remains usable under A while old bootstrap aliases refuse. Fence A
and both C/D refuse fresh source work. Repeat own prebind busy/unconfirmed and
unknown publication controls for typed actors as for root/LLM worlds. Fresh
and captured LLM E must remain usable when A/C are fenced, subject to its own
source gate and existing cleanup owner. Pure continuation/readonly receipt
and status operations must remain usable under source refusal.

Status: approved bounded strategy, implementation pending exact leaf candidate;
read-only source advice sent to parent and leaf. No new code, framework, native
build or test execution. Report whitespace check is the only new check.
Awaited producer/event: admission leaf supplies the complete exact R1 candidate
including generalized BootstrapWorld and private initial-uncertainty custody;
parent supplies joined source/evidence when root admits native verification.
Next action is cumulative exact-candidate review of these production aliases
and parent/own claims. Yield without periodic polling.


### Exact cumulative checkpoint fe3ef791f967: blocked source verdict

Reviewed committed base..fe3 production delta, including the prior directory
repair, BootstrapWorld issuance/claims, publisher retained facts, compiler
scopes/tickets, callback captures and the runtime lexical-scope gate. The
admission checkout HEAD had advanced to 47f9442bf9917a9b135a21e5574425ae6ba6b50b
while reviewing, so source observations above used git show/diff of fe3 rather
than worktree contents. No WIP or product source was edited.

Blocking findings:

1. **Uncertain hidden compiler work loses its runtime claim.**
   `resident_workbench.rs::with_continuation_machine` and its wait variant
   deliberately issue `admit_continuation()` tickets with no source claim.
   A completed Binding/Projected continuation can still call
   `resident.rs::complete_prepared` -> `bind_prepared` ->
   `persistent.rs::bind_replacing_decls_in` -> `retract_value_heads_in` ->
   `SessionLib::prepare_retraction_in`, which can perform actual compiler work.
   Its lazy source claim is stored in PersistentSession.source_claim by
   `admit_source_work_in`. At checkout exit,
   `ResidentSession::with_source_admission` (3519 in fe3) unconditionally assigns
   previous_claim, dropping the just-earned claim. It never observes compiler
   END or unwind. `CompilerWorkTicket::consume_close` preserves unconfirmed
   source work only through ticket.source_claim, which is None for this path.
   Counterexample: resume a binding that retracts an existing declaration while
   A is open, let its actual compiler close become unconfirmed, then fence A.
   The generation can report zero claims and reopen/retire despite unconfirmed
   native work. Connect lazily earned runtime claims to the actual owning
   compiler close, including panic/abandonment, without adding a source claim
   to a pure continuation. Both owner and leaf received exact paths. Required
   control pairs clean/uncertain/abandoned hidden binding compilation with a
   pure continuation that earns no source claim.

2. **Nonexistent ownership enum variant prevents actor compilation.**
   `local_actor.rs::SpawnOwnership` declares Independent and Supervised, while
   is_independent (213 in fe3) matches Self::World. The SourceDependency rename
   accidentally changed an unrelated enum consumer. Restore its actual
   Independent variant. This is source evidence, not an observed compiler run.

3. **New admission-history fixture drops actual lexical custody.**
   `generation_admission.rs::tests::generation` drops its local runtime lexical
   scope lease when returning only GenerationDependency. The next generation
   helper invocation calls mint_notebook_scope_with_lease, which reaps queued
   lease retirement and closes that earlier gate. In
   first_visible_world_closes_old_alias_after_replacement, creating next thus
   retires first before first.fence().unwrap; typed-parent controls similarly
   retire parents they expect open. Keep the actual leases in an explicit test
   history owner. The leaf acknowledged and is repairing the fixture; do not
   weaken production retirement or make metadata dependency own lexical custody.

Adjacent source results:

- Actual typed capture now mints BootstrapWorld external Some(parentA), root
  and independently captured/forked LLM worlds use None, and already admitted
  World descriptors survive root handoffs. Preparation selects only external
  prerequisites; C -> D does not accidentally depend on C. Bootstrap claims,
  checkout and registration guards retain parent runtime claims, including
  propagation of uncertainty. SourceCallback and from_context retain the
  issuing dependency, independently of generation() returning None.
- Initial publication is not self-blocked by its own checkout claim in the
  inspected production path: prepare_actor_generation holds bootstrap claims
  only in its synchronous checkout, dropping them before returning prepared
  custody. ActorGenerationPublisher uses with_machine_wait_mode(false), while
  WorldPublication separately retains external A through commit settlement.
  Initialization's compiler-owner scope captures a capability rather than
  automatically admitting a ticket. This is source reachability, not execution.
- No concrete World/runtime reverse-lock cycle found in the actual synchronous
  callbacks: directory generation_enqueue only sends its message; startup
  source admission pushes into the owned source vector; activation admission
  checks no action; compiler preparation admission touches lifecycle metadata.
  These do not acquire World.state from inside an already held runtime gate.
  Runtime chain checks lock ancestors before descendants. Retain an actual
  production race control once native verification is admitted.
- Parent's preleased initial-scope concern remains a latent API hazard without
  a proven production route here. retain_lexical_scope ALWAYS mints a separate
  detached scope before leasing it, so installation/checkpoint capture does not
  lease the child's actual target notebook scope. Child-session provisioning
  uses raw mint_isolated_scope, checkpoint remint produces an unleased target,
  and the actual target's first lease comes from initial notebook adoption.
  Replacing an existing leased gate would strand lower aliases; do not change
  canonical ownership based on this unproven counterexample. A direct public
  runtime API can prelease a scope, but no actor production consumer inspected
  makes the child share A's canonical gate that way.
- InitialPublicationUnconfirmed retains private candidate facts and unbound
  public owner; actual visibility creates ActorGeneration with real readiness.
  Unknown first publication closes old world aliases permanently. Recovery is
  explicit enclosing host/run reopen into a NEW recovered context; no same-run
  fence reset, live reconciliation or prospective bound owner is authorized.
- Directory base..804 repair remains intact: typed starter failure distinguishes
  proven NotStarted and StartUnconfirmed, preserves uncertain identity/pending
  custody, and actual reservation supplies worker preparation provenance.
  fe3 still routes source-dependent RunOwned creation through supervised worker
  reservation; the leaf's announced 47f9442 delta corrects this using existing
  reserve_generation_actor. That later delta is not accepted by this fe3 verdict.

Verification: cumulative git diff --check base..fe3 exits 0. Native production,
unit, host and focused integration compilation/execution are all unqualified;
0 commands executed as tests. Required held registration successors and the
three findings above prevent any complete-R1 acceptance. No R2/R3 changes are
accepted by this review.

Awaited producer/event: admission leaf supplies an exact repaired candidate
covering hidden-END claim custody, ownership enum and fixture leases, including
its final RunOwned/callback/ticket controls; reload supervisor supplies the
joined revision and admitted native evidence. Next action is exact cumulative
repair review and those production histories. No periodic polling.


### Exact repair checkpoint 9bb4c1eb4bf5: bootstrap END custody still blocked

Reviewed committed fe3..9bb and cumulative base..9bb with the earlier fe3
production map. Repair sequence includes 47f9442bf9917a9b135a21e5574425ae6ba6b50b,
f2ab98b069b46495711a0199b9856fdd9fac8916 and
9bb4c1eb4bf5a0775dae68beb119446bb34d06b9. The checkout was clean at exact 9bb.
Only this authorized review report was edited.

Source repairs verified:

- All four fresh/continuation(+wait) machine helpers call run_source_transaction.
  It obtains the actual CompilerWorkTicket's lazy claim collection and invokes
  ResidentSession::with_source_claim_transfer INSIDE the ticket action.
  Transfer runs after the operation returns or unwinds and before rethrow.
  extract-cmd endpoint's owning TransactionScopeGuard performs finish/abandon
  only after that action, so a lazy RuntimeSourceClaim reaches the ticket before
  actual END. Unknown close and abandoned ticket drain those claims through
  retain_unconfirmed. Clean/NotStarted releases them after END; the shared Arc
  holder survives until the synchronous helper returns. Previous runtime source
  claim restoration occurs before transfer/rethrow. Nested compiler transaction
  scopes are explicitly prohibited by the existing owning runtime; nested
  lexical transfer scopes restore the previous claim rather than replacing its
  owner. No new generation-path claim-loss defect found in this ordering.
- The enum repair restores SpawnOwnership::Independent. The History test owner
  retains every actual lexical scope Arc; subsequent mints no longer reap gates
  those histories expect open. Neither repair weakens production semantics.
- Source-dependent RunOwned uses reserve_generation_actor and ActorOwned uses
  reserve_generation_worker, preserving source membership separately from
  lifetime. The existing real reservation/RootWorker provenance chain remains.
- New actual compiler-ticket unknown-END and SourceCallback/from_context alias
  controls inspect the correct owners. Callback transfer's generation() None
  does not erase its source dependency. These tests remain unexecuted.

Remaining confirmed source counterexample: **bootstrap continuation custody is
not transferred to native END.** BootstrapWorld::checkout_admission returns an
own BootstrapClaim alongside an optional external runtime gate. The root/LLM
bootstrap case returns None; the typed bootstrap case returns parent A's gate.
ResidentMachineAccess::with_machine_wait_mode keeps that own claim in local
_bootstrap_claim, outside run_source_transaction/CompilerWorkTicket. Continuation
admission intentionally has no eager source claim. For a root bootstrap,
admit_source_work_in sees source_context_active + source_admission None and
earns no RuntimeSourceClaim at all, so the new lazy transfer collects None.
After hidden compiler END is uncertain, the outer BootstrapClaim drops normally
and the world remains open. In the typed bootstrap case external A's runtime
claim becomes uncertain but the child's own bootstrap obligation still drops
cleanly; own-world accounting is incomplete.

This is production reachable, not a forged context: host
`exomonad/host/src/actor_host.rs` (2320 in 9bb) provisions the OperatorService's
actor with forest.new_workbench. ResidentBoot::Workbench initialization installs
a Bootstrap lease and returns without generation binding. Its real
begin_ready_bind_in_checkout uses run_bind_with_sites/run_projected_bind_with_sites
(8848/8855 in 9bb). An effectful Binding/Projected run can suspend and later
complete through the continuation wrapper, then bind_prepared/retract_heads_in
can perform hidden compiler work. An unknown END on that work has no retained
own-world obligation after checkout. The repair needs the actual bootstrap
source claim tied to the same native END, or an equivalent existing-owner
handoff, while preserving pure continuation behavior. No metadata-only
precheck or compiler lifecycle observation can substitute for the world claim.
Parent and leaf received exact paths before this report.

Detection limitation: the new actor continuation test manually pushes a claim
into the ticket collection, while the new runtime test independently drives
binding plus transfer. These are useful component controls, but both would pass
if run_source_transaction stopped wiring them together. Require the smallest
joined production control through actual continuation helper/run_source_transaction:
actual lazy binding earns the source claim, the real owning ticket receives an
Unconfirmed END, and a source fence reports retained uncertainty. Include
Generation, root Bootstrap(None) and typed Bootstrap(Some(parentA)) worlds,
plus clean/NotStarted and unwind controls. Pair with a pure continuation which
keeps source mutation refused after a fence and earns no RuntimeSourceClaim.
Use existing fixtures/runner and close observation; no production test hook or
new test framework. A joined actual hidden-retraction compiler history remains
behavioral acceptance evidence after admitted native compilation.

Verification: git diff --check base..9bb exits 0. Native compilation and test
execution remain 0; source review is not an executed pass. Required R1
registration successors remain HELD and complete-R1 acceptance remains blocked.
Campaign has approved the bounded lazy source hook repair described below.
Awaited event: admission leaf supplies its exact stable implementation candidate;
parent supplies admitted compile/focused evidence when the lane is available. Next action is
cumulative exact repair review including joined claim/END controls. No polling.


### Approved lazy source hook: bounded oracle and producer guidance

Campaign authorized the smaller scoped hook bridge for the remaining bootstrap
END counterexample. No new implementation was reviewed in this activation:
9bb remains the exact reviewed candidate and remains blocked pending repair.

Owning contract: actual PersistentSession::admit_source_work_in invokes the
scoped hook before source allocation/mutation. Only the existing
ResidentMachineAccess::run_source_transaction installs it. The hook captures
the original actor SourceDependency and the SAME CompilerWorkTicket claim
collection, earns and stores that source claim once for the actual scoped
compiler operation, and ties clean/unknown/abandoned settlement to actual END.
Pure continuation never invokes this hook. The concrete RuntimeSourceAdmission
gate remains authoritative; hook absence supplies no bypass. No registry,
foreign authority trait, new gate, or runtime-to-actor dependency is needed.

Review/test requirements sent to the leaf:

- Invoke hook admission before the current source_claim.is_some early return,
  so a previously earned runtime claim cannot bypass a newly installed scoped
  source owner. Mark the per-operation once-success only after the original
  dependency's affine claim is earned and stored in the real ticket collection.
  Failed admission has no source side effect and supplies no success marker.
- Preserve previous hook, once-state and runtime claim ownership across nested
  lexical scopes and unwind. Each actual transaction must own its own once
  state. Compiler transaction scopes themselves continue to reject nesting
  under the existing extract-cmd owner contract; do not invent nested END.
- Run the smallest joined control through actual with_continuation_machine or
  its production run_source_transaction composition under a real checkout.
  Actual bind/retraction must earn the hook claim; the actual ticket's existing
  aggregate close sink reports Unconfirmed; generation fence/world admission
  then retains uncertainty. Cover selected Generation, root/LLM Bootstrap(None)
  and typed Bootstrap(Some(parentA)). The typed case must preserve own-world
  and parent uncertainty, and clean/NotStarted must release both normally.
- Pair with pure continuation after source refusal: no hook invocation, no new
  source claim and successful receipt/status progress. Pair with callback hook
  refusal before a source allocation, binding count or effect observer changes.
  Retain unwind/abandonment controls through the same wrapper. Component tests
  which separately push a ticket claim and exercise runtime transfer cannot
  detect missing production composition and do not replace this joined control.

Context-gate versus target-scope question remains qualified, not a confirmed
finding. Actual replacement::stage_replacement_inner calls
CallbackExecution::transferred with successor placement while preserving old
issuing dependency A; map_source and run_receiver use that mixed context.
However the replacement target scope begins raw/unleased, and captured/private
scope retention normally preserves the original source chain. No inspected
actual source action was shown to target a DIFFERENT selected own-generation
gate while carrying only A's context. Therefore this activation requests no
new recipient/parent enclosure prerequisite and no speculative target-gate
repair. A future concrete target-scope counterexample should retain its actual
issuer, ownership and operation history before changing applicable authority.

Status: read-only guidance complete; source freeze applies until leaf supplies
an exact committed repair candidate. No product edits, builds or tests. Only
this authorized report was updated; whitespace check passes. Awaited producer:
admission leaf's exact lazy-hook/joined-control checkpoint, followed by reload
supervisor's exact-candidate review activation and admitted native evidence.
Next action: cumulative source review of hook timing, once-state and actual END
custody. Yield without periodic polling.


### Adjacent immutable facade compiler producer: confirmed END custody gap

Parent requested a concrete source audit of start capture while the lazy hook
repair is pending. Exact reviewed source remains 9bb; no WIP acceptance.

Confirmed production chain: resident_workbench.rs::capture_boundary_mode enters
with_continuation_machine (10189 in 9bb). ForkAgent dispatch calls
ResidentActorStart::capture_spawn (10314); ActorStartWith/ActorReplaceWith calls
ResidentActorStart::capture -> capture_decoded (10458 and 10514). Both capture
owners earn only a LOCAL `_source_claim` and invoke materialize_entry_facades.
That helper takes &ResidentSession, reads its immutable SessionCompileView,
and calls surface.materialize (start.rs:524) for actual local exports or
MaterializedFacade::select_exact_exports (527) for inherited narrowed exports.

Runtime session/facade.rs::ExactExportSurface::materialize (103) invokes
issue_projection with the certified baseline. MaterializedFacade::select_exact_exports
(213) invokes issue_projection when the selected exports narrow the original.
These are actual compiler producers. They do not enter mutable
PersistentSession::admit_source_work_in, and the immutable SessionCompileView
does not carry an actual compiler-END claim. The capture function drops its
local source claim on return, before the enclosing continuation ticket settles
native END. Consequently an unconfirmed projection compile can lose its source
obligation, including a bootstrap world's own obligation. A new hook installed
only at mutation entry points does not cover this producer.

This is the same approved source-claim/native-END failure category, not a
request to expand force or resource registration. Parent and leaf received
exact paths. Bounded alternatives are to invoke the SAME scoped hook at actual
immutable facade compiler issuance through its existing runtime owner, or to
transfer the real capture claim explicitly to the SAME existing compiler
ticket. Either must retain normal refusal-before-effects and own-world plus
external prerequisite uncertainty; another local precheck is insufficient.
Parent owns the choice. No new authority trait, registry or independent gate
is implied.

Required joined history: drive actual ActorStart/ForkAgent capture carrying at
least one real local facade or an inherited facade requiring narrowing, observe
actual projection compilation, report aggregate native-END Unconfirmed and
verify original world/generation remains uncertain. Empty rooted sets and
unchanged inherited selections are separate no-compiler controls, and cannot
certify coverage of this producer. Pair clean END and cancelled/abandoned
capture outcomes, preserving actual child preparation/custody ownership.

Status: confirmed source blocker on 9bb and a required coverage condition for
the pending repair, not an executed regression. Awaited event: reload
supervisor chooses bounded facade hook/handoff seam, leaf includes it in the
exact stable lazy-hook repair checkpoint, then explicit exact review activation.
No code/build/test execution and no periodic polling.


### Exact frozen checkpoint fc92605eb78c: cumulative source review complete

Reviewed full campaign base..fc92605 with the prior cumulative issuance map,
focusing on exact 9bb..fc92605 compiler-hook and immutable-facade repair. The
admission checkout was clean and HEAD exactly
fc92605eb78c52284f90249f6e5dd1200ba060de. No WIP was used. No new confirmed
source defect found in the delivered foundation; native behavior remains
unqualified and required R1 successors remain incomplete.

Confirmed source closure of the prior END findings:

- CompilerWorkTicket now owns run_source_transaction. Every fresh and
  continuation helper, including waiting forms, calls that function with the
  actual original context dependency and actual ticket. Its hook captures the
  SAME ticket's lazy SourceClaim collection. The per-operation success flag is
  set only after dependency.claim succeeds and the affine claim is stored.
  Both the hook closure and runtime admitted flag preserve failed admission.
- PersistentSession::admit_source_work_in invokes the scoped hook before its
  cached-runtime-claim early return and before source mutation/allocation.
  A root/LLM bootstrap with runtime admission None therefore earns an actual
  BootstrapClaim at its first source action. Typed bootstrap retains both its
  own world and parent prerequisite. Pure continuation does not invoke the
  hook, so it does not gain an eager source gate requirement.
- Runtime admission/hook/previous claim are restored on return and unwind.
  Concrete lazy RuntimeSourceClaim transfer remains inside the compiler action.
  extract-cmd TransactionScopeGuard.finish/Drop performs the native END after
  that action returns/rethrows; the actual ticket then preserves eager, lazy
  actor-world and runtime claims on unknown/abandoned close. Clean/NotStarted
  releases them after END. The hook's Arc custody is scoped and restored; no
  registry, new generation gate or runtime-to-actor dependency was introduced.
  Compiler transaction nesting remains explicitly prohibited by the existing
  owner; nested lexical admission/hook restoration preserves prior owners.
- ExactExportSurface::materialize and MaterializedFacade::select_exact_exports
  are now private within runtime session. New Session/PersistentSession wrappers
  invoke the same scoped hook immediately before actual projection issuance
  or changed legacy source writes. Wrong-session/empty/unchanged selection
  paths remain metadata/no-production controls. Actual
  start::materialize_entry_facades uses these wrappers for both local and
  narrowed inherited projections. Repository source search found no surviving
  external consumer of the removed public object producer methods. The local
  capture guard can drop before END because the actual producer hook has
  already stored an independent affine claim in the same ticket.

Other cumulative relationships remain intact: explicit Unqualified generic
constructors; one canonical BootstrapWorld at real root/LLM/typed capture;
typed parent A plus own C and replacement D excluding C; old bootstrap aliases
forever first-C-bound; SourceCallback/from_context original dependency preserved
even with generation() None; immutable selected snapshot separate from current
installation; actual reservation/worker preparation provenance; RunOwned source
membership separate from supervision lifetime; canonical runtime lexical gate
retained independently of physical code/custody. Initial publication unknown
retains private candidate facts and an unbound public owner, permanently closes
old world aliases, and uses explicit enclosing host/run reopen into a NEW
recovered context. No live native-store fence reset or fabricated bound owner.
The earlier context-versus-different-target-gate and preleased-initial-target
questions remain qualified: no actual production counterexample established.
No speculative enclosing-actor dependency was added.

Oracle assessment: the two new joined actor cases call the real owning
run_source_transaction. A legacy Haskell Sleep binding suspends before
completion; resumption actually binds under Generation, root Bootstrap(None)
and typed Bootstrap(Some(parentA)); injected aggregate Unconfirmed then leaves
the exact own owner uncertain. Pure continuation is checked claim-free, and
bootstrap hook refusal precedes native export/binding changes. The augmented
start capture control drives the real materialize_entry_facades helper with
local and narrowed inherited selections; empty/unchanged selections stay open.
Removing the actual wrapper/hook wiring would break these controls, unlike the
earlier split manual-vector/runtime-transfer tests. These remain SOURCE tests:
no native compilation, case execution or actual post-resume GHC END fault was
observed by this review. A real hidden-retraction compiler history and hosted
self-reload/Ready-renderer/independent-collateral behavior remain execution
obligations, not inferred passing evidence.

Evidence inspected:
`/srv/swarm/evidence/inanna/persona-1895-launch-20261019/compiler-failure/reload-admission-20261019/HANDOFF.md`,
`source-checks.json` and `DEFERRED-COMMANDS.txt`.
The source-check receipt names exact fc92605 and records cumulative whitespace,
pinned rustfmt check and first-party source-mapping check exits 0. Actor/runtime
BUCK mappings include the new owning modules. Reviewer independently ran
cumulative git diff --check base..fc92605, exit 0. All native compile/test counts
are 0; no claim here depends on a zero-match selection or target linking.

Deferred verification is concrete in DEFERRED-COMMANDS.txt: pinned configure,
then runtime/actor production and unit targets, followed by the counted source
admission, callback, joined continuation and actual facade selectors. Also
retain the actor guide's affected hosted-consumer check and existing counted
notebook integration cohort when the joined R1 tranche is ready. Required
WatchRoute/forms/reply-tool/display registration successors remain HELD until
first runtime+actor production/unit compilation. Their known absent provenance
and execution evidence prevent complete-R1 acceptance; R2/R3 remain held.

Campaign has now admitted the parent-owned runtime/actor production and unit
native gate after this review, area fold and source generation, with 12 GiB
memory and 32 GiB prospective disk growth. No native launch or result exists at
this review receipt. Awaited producer/event: reload supervisor supplies the
exact integrated source checkpoint, compiles actual production and unit
consumers through that admitted gate, and executes recorded nonzero focused
cases. Next reviewer
action is concrete compile/test-failure repair review or cumulative review of
an explicitly authorized R1 successor candidate. Until that event this source
review is complete, not runtime acceptance. Yield without periodic polling.


### Public runtime owner boundary: exact counterexample and decision packet

Root requested a bounded runtime-only audit at frozen fc92605 (area checkpoint
7d), while parent native build 32799 runs. No code or build was touched. The
prior actor same-ticket review remains valid for actual start.rs production
consumers; it does not certify the separate PUBLIC runtime API contract.

Minimal runtime-only counterexample: create a real leased lexical scope S and
certified nonempty ExactExportSurface with its SessionCompileView. Enter the
existing runtime-reexported settle_compiler_transaction_cancellable_for_workload.
Inside its action, call PUBLIC PersistentSession::materialize_facade (or narrowed
select_facade_exact_exports) with no actor ticket/hook. admit_facade_source
obtains the concrete S RuntimeSourceClaim. The private object method drops it
when projection issuance returns. Fence S before the OUTER action returns:
pending_claims is zero and retirement can succeed while the compiler scope
still owns physical work and has not performed END. If that END later becomes
Unconfirmed, no S claim remains for its owner to retain. No forged actor grant
or generation label is needed.

Exact owner distinction: extract-cmd endpoint.rs around 1603 returns a borrowed
operation outcome with close NotStarted whenever TRANSACTION_SCOPE already
exists. That is not the enclosing physical END. TransactionScopeGuard.finish
(2085) and Drop (2095) perform actual finish_scope after the outer action; its
aggregate includes reported/admission uncertainty. CompilerTransactionOrigin
is identity evidence only, not standalone source admission. Callback settlement
reports and a current origin cannot replace ownership of that final callback.
Unscoped one-shot invocation also needs to retain an uncertain close rather
than drop its scope claim after observing it.

Root/Astra approved the bounded direction: one runtime-owned source compiler
transaction entry, bundling source admission, lower claim retention and actual
END settlement. No new registry/gate/framework, foreign authority trait,
runtime-to-actor edge or global source graph is required. Exact signature is to
be coordinated by campaign BEFORE implementation and AFTER the frozen build.

Minimal proposed shared seam (names illustrative, not implemented):

- One public ResidentSession::with_source_compiler_transaction taking existing
  workload, cancellation, original source selection, optional supplemental
  issuer hook, actual-close sink and synchronous action. It returns the actual
  CompilerTransactionSettled product from the EXISTING extract-cmd settle
  owner. Keep with_source_admission, with_source_admission_hook and
  with_source_claim_transfer private implementation details. The supplemental
  hook is an additional restriction; absence or a callable Ok result cannot
  waive concrete lower admission or manufacture an active owner.
- A private active runtime transaction record binds the real current native
  origin and session admission owner to the SAME lower-claim collector. The
  runtime entry creates it inside its actual owning native scope, scopes it
  through the action, and retains its collector in the actual close sink.
  Claim settlement happens there: unknown/abandoned retains uncertainty;
  clean/NotStarted releases after physical END. Metadata origin equality alone
  cannot construct this record. Preserve prior lexical selection/owner on
  unwind without discarding the collector.
- Public facade producers require that actual active runtime owner or return
  typed MissingSourceTransaction BEFORE compiler/source effects. They append
  concrete claims to its collector instead of returning a local RAII claim.
  Their private object producers receive no publicly replaceable Ok(None)
  admission callback. Private fixture shortcuts must migrate to the real
  public owner when claiming boundary coverage.
- CompilerWorkTicket::run_source_transaction delegates to this runtime entry
  rather than wrapping another ticket.run around it. The actor ticket keeps
  actor/bootstrap obligations and cleanup; runtime owns lower source claims.
  The runtime actual-close sink settles lower obligations then invokes the
  SAME actor ticket's actual close callback exactly once. Existing workload,
  cancellation and reported-close aggregation remain authoritative.

Borrowed/nested discipline is essential: facade/projection calls inside the
active source compiler transaction borrow its real native scope and retain
claims through that OUTER END. Their local NotStarted observation never settles
the collector. An owning source-entry call under a foreign raw compiler scope
without a matching runtime source owner must typed-refuse, not create a nested
native scope or fake an END. If explicit lexical reborrowing of the same source
owner is supported, reuse the exact collector and do not issue a settled result
before its outer close. A smaller enforceable API may reject nested owning
entries entirely; ordinary inner producer borrowing must still work. Existing
native transaction scopes already prohibit nesting. Do not loosen that
contract to make a convenience wrapper appear compositional.

Consumer/dependency scope: current non-test actor calls are only
start::materialize_entry_facades local and narrowed inherited paths, already
under the safe actor wrapper. Runtime facade/turn and actor mount/start fixture
callers must migrate with the owning public API. The runtime already depends
on extract-cmd and exports its compiler settlement primitives; the actor already
depends on runtime. This repair moves existing lower custody to its correct
native-close owner without an edge inversion. No change to serialization,
source identity, resource lifetime, generation gate identity or independent
LLM source semantics is implied. Source-selection arguments must preserve
approved bootstrap independence without offering a public None/hook-absence
route around applicable lower checks; campaign owns that exact API decision.

Required public-owner controls: missing active owner refuses before compiler
requests/files; real nonempty/narrowed facade under the owner keeps an observable
pending S claim AFTER the producer returns but BEFORE outer END; clean END
releases; actual owner Unknown/abandonment preserves; panic restores lexical
state and keeps closure custody; retired scope refuses before effects. Exercise
multiple borrowed producer calls and observe no early settlement from their
local NotStarted reports. Keep synthetic aggregate-Unknown and actual physical
END uncertainty evidence distinct. Empty/unchanged facade selections remain
separate metadata controls, not substitutes for the claimed compiler boundary.

Verdict: bounded repair within existing compiler/runtime ownership, not an
architectural hard gap. Frozen fc92605 actor wrapper is source-safe, PUBLIC
runtime boundary is not yet sufficient. Parent/campaign receive this decision
packet; exact implementation review follows a separately frozen candidate.
Only this authorized report changed, whitespace check passes; no new native
commands or tests executed. Awaited event: campaign settles the shared source
transaction signature after the current frozen gate, leaf/owner implements it,
and parent supplies an exact review checkpoint. Yield without polling.


#### Source-selection ambiguity: explicit non-optional option for campaign

Parent requested an exact enforceable option rather than treating a public
None plus absent/present callback as independence evidence. Minimal proposed
selection is either just an existing RuntimeLexicalScopeLease reference, or
`RuntimeCompilerSource::{Current, Retained(&RuntimeLexicalScopeLease)}`.
Current resolves the canonical source lease for the actual selected runtime
source owner; Retained validates that existing opaque lease against its ORIGINAL
issuing session admission owner and epoch. A transferred callback can execute in
a different recipient session; equality with the current recipient session would
be incorrect. Validate recipient placement/root custody separately. Both select
a real canonical gate and refuse absent/dead custody, without eagerly requiring
the gate's Open phase for pure continuation.
There is NO public Independent/Bootstrap/None permission variant. The optional
actor hook remains a supplemental restriction, never an authority substitute.
A naked Fn()->Ok cannot waive lower checks or issue an active source owner.

This requires actual root/LLM bootstrap placement owners to obtain the initial
canonical notebook lease BEFORE their first source compiler operation through
EXISTING adopt_initial_notebook_scope_with_source (or the existing equivalent
lexical lease issuer), and retain it through existing startup custody. Typed
bootstrap uses external A; independent root/LLM uses external None at that
actual issuer. Later first generation preparation reuses the SAME lease/gate;
BootstrapWorld still binds its aliases only at actual visibility. No candidate
is selected early, no additional gate domain/factory is introduced and no
already leased gate is replaced. A captured unleased inherited record can be
normalized only at the actual initial-world placement issuer before lower
aliases are exposed, preserving the earlier canonical-owner rule. Native
pre-world preparation stays under its actual existing preparation owner.

This option changes existing issuance TIMING and startup lease custody and
therefore needs explicit campaign approval before implementation. Without
that option, the combination of actor-private bootstrap proof, no new runtime
capability factory/type and a publicly constructible Fn hook does not yield an
enforceable proof that runtime None means independent authority. That is a
bounded unresolved source-selection decision; the rest of the runtime native
END owner mechanism is not an architectural gap. Do not silently resolve it by
adding enclosing parent dependencies or trusting callback presence alone.

Raw with_machine read-only/pure context selection must also migrate
deliberately: retain an observation-only placement/source selection that cannot
issue compiler permission, separate from the owning compiler entry. Keep its
selection setter private; route all actual source work through the source
compiler transaction. The three stacked public composition helpers cannot
remain an alternate native source-authority route after the owning entry lands.

The leaf's separate public-facade fixture migration checkpoint
89ceb83b785168fa855817d67863f586efce60e8 was RECEIVED, not reviewed here. Its
real retired-scope/missing-owner/unknown-END tests must consume the final
approved owner API; no mock Ok hook can certify that API. Exact parent/campaign
signature decision is the awaited producer event before an implementation
checkpoint is requested.


#### Final placement-issuer feasibility packet: frozen fc92605 plus 426718622e

This is the requested bounded SOURCE-SELECTION timing review, read from exact
committed objects. Area 426718622e moves the public facade delegates into the
ResidentSession state-owning module; it changes neither issuance nor settlement.
No WIP, native command, or product source edit was used. The proposal below is
NOT implemented or approved merely by this review.

| Actual production path | Earliest existing canonical lease issuer and required custody |
| --- | --- |
| Operator/root workbench | `ResidentActorForest::new_workbench` (`resident_actor.rs:14963`), called by `exomonad/host/src/actor_host.rs:2320`, obtains its final non-ROOT isolated scope through `ResidentActorRunner::provision_root_scope_inner` (`resident_workbench.rs:12775`) and the existing RootPlacementMint custody. Existing `adopt_initial_notebook_scope_with_source(final_scope, None)` can issue here, before source work, or after actual placement admission and before boot. The resulting exact Arc must travel in existing placement/startup/behavior custody; a local temporary Arc is insufficient. Workbench boot installs bootstrap tools and publishes no first generation. |
| Program root / program replay | `new_program_root_with_replay` (`resident_actor.rs:14794`) first calls `prepare_root_program_tracked` (`resident_workbench.rs:12642`). That function mints the final isolated scope and runs the authored root entry before constructing the actor descriptor/world. Adoption must occur at that mint before `run_with_sites`, with exact custody returned through existing RootPreparation/placement owner; adopting only in Behavior::start is too late. Pre-session/native setup remains under its actual preparation compiler owner. |
| Host pending/recovered durable root | `actor_host.rs:3599–3617` mints the final isolated scope, prepares the startup entry and seals initialization. `admit_pending_root_inner` (`resident_actor.rs:15132`) retains actual reservation/startup custody, while Behavior::start deliberately does not execute the entry until `release_root_startup` (`15299`; host `2213`). On recovery the actual public-owner transfer (`actor_host.rs:2158`; runtime `persistent.rs:3043–3055`) advances RuntimeAdmissionOwner epoch. A lease retained before that transfer becomes stale; the canonical map will not safely replace that live lease. Earliest safe retained lease is AFTER confirmed public-owner transfer/application binding, BEFORE startup release/authored resume. Old pre-recovery lease aliases must remain stale. |
| Independent fresh LLM on a new machine | `start::capture_spawn` can supply a different session and placeholder ScopeROOT. `child_launch::await_launch` (`365`, provision call `515`) invokes `provision_child_session` (`resident_workbench.rs:5015`). Its factory runs inside an existing CompilerWorkTicket::run before any machine/final scope exists; only afterward does `mint_isolated_scope` create the real target and `run_with_sites("child_session_bootstrap", ...)` run. ScopeROOT adoption is rejected. Adopt the actual newly minted target with external None before bootstrap/source aliases, and retain it through ProvisionedChildSession/actual startup custody. A nonoptional-lease transaction entry CANNOT cover the factory prefix unchanged: no such lease exists yet, and nested entry under the current ticket.run would borrow the wrong physical-END owner. |
| Shared/captured independent LLM | Capture may mint a provisional scope, but `await_launch:486` can replace it via `remint_checkpoint_child_scope_from_lease` (`resident_workbench.rs:10055`), or `await_launch:502` via fallback provisioning (`12817`). Adopt only the resolved final placement, external None, after remint/fallback and BEFORE `transfer_mailbox_value` (`child_launch.rs:532`). Adopting the provisional scope then silently substituting another canonical gate strands aliases. Retain the exact lease through child placement/startup custody into Behavior. |
| Typed child | Same final-placement/provisioning rules, but capture's `admit_dependent_world(parent.selected_dependency())` supplies external A. Its own canonical initial C gate must be issued with that external prerequisite. First generation C and later D preserve external A; D must not acquire C as an incidental enclosure prerequisite. Capture-time parent facade materialization belongs to the parent's owner before child placement exists; it does not issue the child's own gate. |

Existing runtime issuer facts constrain these placements: `prepared_notebook_scope_lease`
(`admission.rs:2025`) normalizes only an inherited record WITHOUT a live lease,
then obtains the canonical scope lease. `prepared_lexical_scope_lease_with_source`
(`1978`) returns an already live lease only at its original current epoch; it
cannot replace a leased gate to change prerequisites. `retain_lexical_scope`
(`1951`) mints a different retained scope, so it is not a substitute for adopting
the actual notebook scope. Initial adoption in `persistent.rs:2397` rejects
ScopeROOT. This packet relies on these actual final-scope issuers, not permission
to mint authority merely from a scope identifier.

The timing option is feasible for the final actor source phase, with changes to
existing placement/startup custody. It is NOT a universal constructor-time entry
without further work. The factory-before-machine prefix and recovered epoch
advance above are concrete limits. Minimal alternatives for the factory prefix
are (a) retain its existing explicit preparation owner, finish that physical
transaction cleanly, then enter the runtime-owned source transaction after
machine/scope adoption; or (b) approve an explicit handoff into the SAME existing
physical transaction's actual final-close owner. This review does not certify
that either is a drop-in API change: campaign must choose and the producer must
show its actual returned custody/settlement before implementation acceptance.
Do not turn an absent lease or optional hook into public independent permission.

Earlier canonical issuance also changes the first-publication cutoff. At frozen
fc92605 BootstrapWorld in `generation_admission.rs:63–116` accounts only its
private bootstrap claims and optional external prerequisite; its lower admission
selection at `208` is external/None until Bound. This is coherent only while
no early public canonical bootstrap lease is exposed. With the timing option,
World claims and runtime selection must use the SAME canonical C gate. Otherwise
a public runtime caller can use C while World Publishing is closed, or World
can admit after C has retained unknown work. `ActorGenerationPublisher::publish`
(`resident_workbench.rs:1897–1923`) enters metadata checkout with continuation
mode false, then begins publication before commit IO. It must fence that SAME
C gate at begin-publication, after atomically stopping World issuance, without
claiming C itself; retaining external A is valid. Outstanding C claims must
settle or cause refusal before uncertain commit, rather than waiting on a
publisher's own C claim. Keep the exact fence token closed through actual
visibility and publication acknowledgement. Confirmed success reopens that
same gate for initial A; proven no-effect/nonpublication may reopen the original
bootstrap gate. Unknown commit retains the exact closed fence permanently.
The current unknown-only `WorldPublication::drop` fence is insufficient for this
new early-public-lease exposure. Candidate preparation must not bind World;
visibility still binds old bootstrap aliases once. Reopen-only recovery creates
a NEW context from actual selected head/readiness and never reuses the old gate,
prospective ActorGeneration, or fabricated public-owner binding.

Pure continuation needs exact strong custody, not eager Open validation.
GenerationDependency currently retains a RuntimeSourceAdmission and source but
not RuntimeLexicalScopeLease (`generation_admission.rs:373–412`). ActorGeneration
owns the scope Arc, and CallbackExecution::transferred preserves a scope Arc
from its explicit scope or Generation snapshot; however `from_context` has
neither snapshot nor scope. Therefore there is no complete proof that all
source-dependent immutable aliases retain a lease after the old generation
wrapper is dropped. A minimal existing-owner repair is to retain the exact
scope Arc in SourceDependency's immutable generation/bootstrap owner, without
an ActorGeneration backreference or cycle. It must survive the startup custody
handoff, first publication, replacement and callback transfer. Validate opaque
identity and ORIGINAL owner's admission epoch at entry; inspect Open/Retired
only at lazy fresh-source admission. Retired source admission can still retain
live lexical/native custody needed for admitted continuing self-reload and pure
callback progress.

Actual cross-session counterexample to recipient-session equality:
`resident_actor/replacement.rs:520` imports a dynamic source entry into recipient
session B, then `CallbackExecution::transferred` preserves original dependency
A while using recipient context B. Delivery and map_source use that mixed
context. Requiring source lease A's admission owner to equal executing session
B would reject a legitimate transferred callback even while A is open. Original
source owner/epoch and recipient physical root placement are distinct checks.
Keep read-only/pure with_machine context selection private and independent of
fresh-source permission; no source claim is earned by metadata observation or
pure continuation. Hidden binding/materialization must still earn the original
source claim lazily under the actual runtime transaction's END owner.

Required discriminating controls for the final owner implementation: each real
issuer above must produce the exact lease subsequently used by first-generation
preparation; recovered startup must show post-transfer epoch and stale old
alias refusal; final remint must not reuse provisional scope; fresh factory
prefix must expose its actual END owner before bootstrap work; typed A+C then
A+D and independent LLM must retain exact prerequisites; direct public lower
source admission must refuse during Publishing and forever after unknown
publication; confirmed nonpublication may retry with the SAME gate; pure
continuation and transferred generation-None/from_context callbacks must run
with retired source admission but valid original custody, and hidden source
work must refuse. Keep synthetic aggregate-Unknown controls separate from an
actual physical END-loss history.

Disposition: the accepted runtime-END owner direction remains a bounded repair.
The nonoptional-lease source selection timing option is conditional on campaign's
explicit prefix/handoff and publication/custody decision; it is not currently
an implemented boundary. Frozen fc92605 still has the documented public-runtime
END gap. This source-only task is complete; parent owns the running native
compile gate and its evidence. Awaited event/producer: campaign decides this
shared signature/timing cone, admission leaf implements the approved owner,
and reload parent supplies an exact committed checkpoint for independent
review. No periodic polling; no native execution was performed by this reviewer.


### Executed Ready-install failure: independent analogue search at f147c79413

Activation/base/candidate: authority_review, campaign base
5b74c2f1fe010c5c343e0c6f331d54328fbd5090, exact area
f147c794130203ff9bf21096cbf6f910f38ea62e. Parent reports the admitted runtime
focus selected five cases: four passed and
`session::resident::authored_publication_tests::source_fence_keeps_precompiled_plain_execution_and_refuses_ready_install`
failed its typed-refusal assertion at resident.rs:9873. This reviewer inspected
committed source only and launched no native command. The failed assertion did
NOT print the actual Result, so it establishes failure to match the intended
error, not whether the result was Ok, Ok(None), a different error, or a partial
installation. The leaf owns the approved Ready gate/diagnostic repair. No final
source-transaction API repair is implemented by this activation.

Confirmed original route: public ResidentSession::revalidate_and_run_prepared
(`resident.rs:7442–7522`) destructures the Ready capsule and calls
PersistentSession::revalidate_and_install_prepared for Legacy. That method
(`persistent.rs:1504–1530`) stages constructor metadata and calls
PreparedEngine::revalidate_and_install without source admission. The engine
(`prepared.rs:5369–5400`) compares imported facts, then calls the actual codegen
PreparedMachine::install_shared and finish_program_install, publishing native
facts/exports and pinning the program. Matching imports are not current source
permission. The certified branch instead reaches install_certified_turn_in
(`persistent.rs:904`) which earns a source claim before metadata and native
installation. The ordinary install_prepared (`1435`) also earns a claim before
metadata/bootstrap/install.

A narrow repair must use the Ready capsule's actual `lexical_scope`, already
available in resident.rs:7469. A check only against ScopeROOT in the persistent
legacy helper can pass the existing test's explicit ambient source override
while missing a direct PUBLIC resident call executing target S without that
override. Validate/gate the actual selected source owner and target where
applicable; do not reconstruct S from ROOT or treat an inherited parent as the
child's own gate. The leaf acknowledged using explicit metadata lexical_scope.
The direct-public no-with_source_admission control remains important alongside
actor-wrapper controls.

Confirmed adjacent production omission: off-checkout native compilation has no
source owner. `begin_ready_block_split` (`resident_workbench.rs:8731`) snapshots
with access.with_machine, then (`8746`) invokes public
PendingPreparedInstall::compile_off_checkout on a blocking thread outside a
compiler/source ticket. Runtime `snapshot_run_prepared` (`7396`) calls
admit_turn_code_in (`6731`), which resolves immutable certification facts only;
no fresh source claim is earned or retained in the pending capsule. The capsule
(`2725`) calls PendingPreparedSource::compile_off_checkout: Legacy invokes
PreparedEngine::compile_off_checkout; Certified invokes compile_scoped. The
legacy lower method (`prepared.rs:5346`) uses a cached image when present, or
calls snapshot.compile.compile / registry.get_or_compile on an actual cache
miss. The capsule carries no source capability/lease/claim to check or retain
at this new compilation. A real pending snapshot can outlive a fence and start
fresh Cranelift compilation after it, even when final installation is repaired
to refuse. A pre-fence admitted compiler action may finish, but this capsule
currently carries no affine admission proving such an action through compile
and settlement. This is the same fresh compiler/install category, not a new
resource registry or R2 obligation. Parent and leaf received the exact path;
leaf keeps the current patch bounded and retains this successor for the final
actual-owner contract.

Additional public-runtime ordering analogue, with the actual actor path
protected: `mount_host_value_with_interface_in` (`resident.rs:6206`) calls
CertifiedTargetImage::compile_scoped (`6231`) BEFORE install_certified_turn_in
(`6234`) earns its source claim. Public mount_json_binding_in,
mount_text_binding_in and mount_typed_binding_in reach this method with geometry
and type checks only. A direct public call with a genuine compiled carrier can
perform an uncached compile before later discovering a closed source gate.
Actual production mount_checked_host_input at resident_workbench.rs:7667 is
under with_compiler_machine, whose eager SourceDependency claim protects this
actor route; do not report it as another unguarded actor-wrapper path. Legacy
mount_carrier_in also writes its stub .hs before its later mount gate and
removes it on error; refusal-after-write is not refusal-before-effects. Legacy
workbench mount helpers found in this bounded search are cfg(test), whereas
checked host input is the actual actor consumer. Public lower API ordering
still needs an actual source-owner control; the Ready repair does not certify
it. Checked host interface staging can write an interface before its later
install, likewise protected by the actual actor wrapper and requiring the
final public-runtime owner boundary if independently callable.

Ready renderer/cache findings: run_activation_preview validates exact original
mount/proof/session epoch and uses a retained detached scope; it then calls
run_prepared_for_purpose with TurnImageAcquisition::Ready. That reaches
install_admitted_turn (`resident.rs:7168`), which earns the source claim BEFORE
lookup_scoped/cache-image access and certified install. Cache hits in ordinary
PreparedEngine::compile_and_install (`prepared.rs:3680–3718`) are reached from
the already gated persistent install; reuse of immutable code still performs a
fresh install_shared, so it must remain gated. No second missing-gate ready
renderer install route was confirmed in this search.

Intentional pure paths: run_startup_entry consumes its already installed
ProgramId and retained native custody without installing or compiling, then
complete_prepared in Value mode releases the result handle. It must remain
usable after source fencing while exact physical scope/root custody is live.
Resumed pure/Value continuations likewise need no eager compiler/source claim.
Binding/Projected completion reaches bind_prepared and the gated persistent
bind owner; it is a fresh mutation and cannot borrow Value-mode neutrality.
Do not add an eager gate to all resume/run_startup entry points to repair the
installers. Retirement/cleanup releases and metadata observation remain
separate from new source authority.

Minimal discriminating controls for the owner repair:

- Print Debug of the actual failing Ready Result. Keep a positive fresh Ready
  install under open admission using an equivalent new affine capsule. Assert
  the closed case's typed SourceAdmission refusal and compare constructor
  metadata, exports, binding count and parked state before/after. This
  distinguishes Ok execution, stale-import Ok(None), pre-existing fixture
  failure and install refusal. The leaf explicitly acknowledged this diagnostic
  and a positive control before the fence.
- Run the same lower public Ready refusal without an ambient source override,
  with actual capsule scope S fenced. Run certified and legacy variants and an
  already warm image; all new installs require the applicable source claim.
- Snapshot an uncached distinct turn under a real retained scope and actual
  machine, fence before compile_off_checkout, and require a typed refusal
  BEFORE compilation/cache publication unless the pending producer genuinely
  owns a pre-fence claim through that whole operation. Observe compile/registry
  work using the existing counters/identity, not elapsed time. Test both native
  Legacy and Certified producer branches. A pre-fence admitted compile must
  retain its actual owner through cancellation/unknown close as applicable.
- Through the final public owner, materialize a real host carrier with an
  uncached native target after fencing and require refusal before interface
  files, compiler/image publication and native installs. Preserve its open
  positive and no-effects invalid-carrier controls; do not fake a permissive
  hook or call private producers to claim boundary coverage.
- Preserve the existing installed startup Value result and resumed pure
  continuation controls, including zero pending source claims. Pair with a
  hidden Binding completion refusal so purity does not hide a source mutation.

Untested boundaries: no analogue controls above were executed by this reviewer.
The parent five-case receipt is not a suite pass, and its one failure is not a
successful bypass execution receipt. Public raw PreparedEngine/codegen access
is lower physical-machine authority, not by itself proof of an actor-generation
route; install_certified_demand had no non-test consumer in this bounded call
search. Pre-world preparation and PreparedSourceEntry standalone immutable
original-code preparation remain distinct issuer contracts; no dependent actor
bypass was inferred merely from their public visibility. Runtime/actor final
transaction-owner, actual physical END uncertainty and registration successors
remain unqualified as previously recorded.

Disposition/next event: concrete same-category omissions were delivered to
reload parent and admission leaf. Current bounded Ready repair is the leaf's
producer; its exact committed candidate and parent-owned focused native receipt
are the next review event. Final off-checkout/public mount owner migration needs
the campaign's pending actual-END/source-selection contract. No code except
this report changed, report whitespace check passes, and no reviewer build/test
ran. Yield without periodic polling.


### Partial source review f982716fdd901616d31e11b810024c766a41fbc3

Exact leaf checkpoint reviewed from committed admission objects:
f982716fdd901616d31e11b810024c766a41fbc3, full delta from frozen
fc92605eb78c52284f90249f6e5dd1200ba060de, including public-facade fixture migration
89ceb83b785168fa855817d67863f586efce60e8. Compared directly against area-only
426718622e43577db1629a6e3040847ef39fe4bd (ResidentSession delegate placement),
ab4cbfda55441090548f2145440ca448967a2a66 (fixture field/reexports) and
f147c794130203ff9bf21096cbf6f910f38ea62e (unsupported force refusal). No product
or fixture edit was performed by this reviewer. Leaf delta whitespace check
fc92605..f982 passed. Parent reports all prior native commands settled: actor
selection 7 pass / 4 fail (missing Sleep fixture minutes, NativeGroupUnavailable
before the mixed-grandchild hook, two original-context commitment mismatches).
Those failures remain real qualification blockers, not waived oracle outcomes;
this source review did not execute or independently reproduce those commands.

Product repair disposition: ACCEPTED for the bounded missing Legacy Ready
scope-gate defect by source review only. Public
ResidentSession::revalidate_and_run_prepared passes the Ready metadata's exact
lexical_scope to the existing persistent legacy installer. It is the only
production caller of revalidate_and_install_prepared. That lower owner now
calls admit_source_work_in(scope) before stage_install_metadata, engine
revalidation, install_shared, export/evidence publication or pinning. This
closes the prior actual capsule-scope-vs-ROOT omission without adding a registry,
new generation owner or eager pure-continuation gate. Certified Ready continues
to use its already gated installer. The admission claim still follows the
existing collector/local lifetime: the separately approved actual-END runtime
owner repair remains necessary and is not certified by this patch.

Confirmed fixture finding at exact f982: the strengthened test changes current
placement to SessionRunContext::ROOT before fencing scope S, correctly forcing
the negative path to consult the capsule's scope without ambient override. It
then invokes run_startup_entry on the entry prepared under S without restoring
that original placement. run_startup_entry compares current.scope against
entry.admitted.scope and returns StaleStartupEntry before evaluation. Thus the
final unwrap cannot demonstrate pure precompiled progress at this candidate.
Small repair: restore the exact original S run context AFTER negative/no-effects
assertions and BEFORE run_startup_entry, preserving the same fenced gate and
retained scope. This finding was sent directly to leaf and parent immediately;
no native failure for f982 was inferred or fabricated. The positive fresh Ready
control and fresh negative snapshot ordering otherwise distinguish a working
image/import path from stale-import Ok(None), and Debug output now preserves
the actual Result on a mismatched typed refusal. Exports, constructor metadata,
binding names and parked count comparisons are meaningful observable no-effects
controls. Retain these after repairing the placement fixture.

Public facade fixture migration disposition: ACCEPTED by bounded source review.
The certified inherited/subset fixture retains a real notebook scope Arc,
defines/extracts actual declarations there, and invokes public
PersistentSession materialize_facade/select_facade_exact_exports for full,
unchanged and narrowed interfaces. No private producer or permissive Ok(None)
admission callback remains in its tests. After source retirement it keeps the
physical scope live and requires exact typed Retired refusal for fresh
materialization and narrowed selection. Unchanged exact selection returns the
original projection Arc; empty selection returns None; wrong-session and
unselected-export controls retain their distinct typed refusals. Source ordering
shows each refusal occurs before issue_projection, with the settlement counter
remaining zero. These are synchronous source/refusal/pure-selection controls;
the counter alone is not an independent physical-END or disk-write oracle.
No uncertainty retention/actual-END acceptance is claimed.

The legacy facade test now constructs a real SessionLib/PersistentSession view
and calls the public materializer instead of fabricating a view and calling a
private producer. It checks rendered source/file content for an uncertified
legacy facade. It does not retain a canonical generation scope or exercise a
compiler transaction, so it is an honest legacy rendering control, not a
certificate for the final source owner. The actual END/public source-owner
migration must update the relevant tests again to enter that final owner; this
checkpoint does not make the still-public local-claim boundary sufficient.

Composition observations: f982 is based on fc926 and still places the unchanged
ResidentSession delegate impl in facade.rs; area's 426718622e intentionally
moves it into resident.rs to resolve sibling-private state access. Preserve that
move when folding, without duplicating or restoring the old delegate impl.
The new fixture edits do not require changing the delegate API. Area ab4's
host_authority fixture field and reexport deletion and f147's pre-preparation
force refusal are disjoint from this candidate's runtime gate and tests; do not
lose them when integrating. Standalone leaf compilation is not established by
this source comparison.

Final partial disposition: product Legacy Ready repair and public fixture
producer migration have no further confirmed source finding in this bounded
review. The strengthened Ready test has the concrete placement defect above;
its control is not source-accepted until repaired. No full R1 acceptance,
compilation/test pass, physical-END custody acceptance or integrated readiness
follows. Off-checkout pending compilation, public runtime owner/public mount
ordering, the four parent actor failures and held registration/R2/R3 work remain
as previously recorded. Awaited event/producer: leaf supplies exact fixture
repair checkpoint, parent folds with area fixes and obtains native evidence;
next runtime-owner implementation review remains reserved. No periodic polling
and no reviewer native action.


Exact bounded repair re-review: 4e790ab850f4df886fc185e15a7b28139f6bf530
on f982 was supplied during this activation and read as committed objects. Its
only product-tree delta restores SessionRunContext with lexical_scope S and the
same ROOT resource/principal defaults originally used to create the startup
entry, after negative/no-effects assertions and before pure entry execution.
It preserves the exact existing source fence and scope Arc; no source gate is
reopened and no product installer changes. This resolves the fixture finding
above by source inspection. Full fc926..4e790 whitespace check passed. Bounded
Legacy Ready repair, strengthened control and public-facade test migration are
now SOURCE-ACCEPTED as this partial parcel, subject to retaining area's delegate
move and direct compile/execution evidence. Leaf reports pinned formatting and
source generation checks only; no leaf or reviewer native execution was
performed for this repair. Parent owns integration and counted execution.
Full R1/runtime-END/off-checkout acceptance remains expressly withheld.


### Active C6 category/contract review at area 345194431d

Exact area base for this activation:
345194431d9f8407a7b7e7c4c273eafbce90bfc0. Read CURRENT reload/HANDOFF.md
(the replaced handoff), actual committed producer/caller objects, and shared C6
decision 923fd554a08ae208833e6bdd9b88a11d54e227ca. The timing/prefix/strong-lease
option previously pending in this report is now APPROVED by campaign: actual
preparation physical END, final canonical lease adoption, then one runtime-owned
source transaction; unknown preparation refuses advancement. This supersedes
older waiting-for-signature status, not the documented defects or qualification
limits. Leaf is implementing, but no WIP implementation is accepted here. Area
native gate is parent-owned and source-frozen; no native command was launched by
this reviewer. Parent reports current four-target gate active (session24915);
no terminal success is inferred.

Primary confirmed new production analogue: checked declaration adoption.
`ResidentActorWorkbench::begin_prepared_cell_item` (`resident_workbench.rs:8369`)
reserves a checked item using ordinary access.with_machine; its Declaration
branch (`8395`) uses another ordinary with_machine and calls
ResidentSession::adopt_checked_declaration (`resident.rs:3995`). That validates
target placement and delegates to PersistentSession's adoption in
`admission.rs:1807`, which checks exact prefix/native products and physical
visibility. It then invokes adopt_staged_declaration_with_activation and the
shared adopt_staged_declaration_prepared (`persistent.rs:2013`). Neither checked
item reservation nor adoption earns source admission. The latter commits the
source-owner origin, declaration binding changes, public visibility epoch and
new declaration tip. Source fencing preserves these physical facts, so retained
compiled declaration products can still mutate the old source domain after
fencing. This is an actual actor route, not an inferred public-only escape and
not pure continuation. The prior with_machine observation contract is necessary
for neutral operations, so making every checkout an eager source gate would be
an incorrect repair.

Minimal independent joined frontdoor control: extend the existing actual test
`resident_workbench::request_tests::planned_cell_defers_declaration_until_its_admitted_item`
(`resident_workbench.rs:25261`). Its real actor registry/private-cell setup calls
prepare_cell on `plannedDecl args = length (args :: [Int])`, obtains the actual
PreparedCell.items[0], verifies no declaration exists yet, then invokes
begin_prepared_cell_item. In a new branch retain the real issuing source A lease,
fence A AFTER prepare_cell returns, keep the physical private scope alive, and
invoke the same production begin_prepared_cell_item with the unused actual item.
Require typed SourceAdmission refusal before declaration-tip/source-origin/binding
changes; inspect the actual prefix cursor/reservation to distinguish clean refusal
from partial reservation. Keep an independent open positive with a new affine
item and source-neutral observation/pure control. An actual pre-fence admitted
transaction may finish, but an unused issued item cannot manufacture a later
source claim. The resulting test must enter the final runtime owner and actual
collector, rather than grant permission through a private test callback.

Related public split frontdoors at the same base:

- stage_declarations_in (`persistent.rs:1910`) and
  render_declaration_candidate_in (`1945`) bypass ordinary declaration commit's
  existing admission. render_admitted_candidate_in (`session/mod.rs:2514`) can
  reserve a declaration generation DURABLY before returning a candidate. That
  durable mutation is fresh work; unattached pure rendering alone is not
  automatically publication or compiler authority.
- Public validate_declaration_candidate (`session/mod.rs:3217`) receives a
  detached candidate, writes its .hs module, invokes compiler validation and,
  when reserved, certifies authored declarations/prepares projection. It carries
  no source capability to check at that actual deferred producer. A candidate
  rendered open and validated after fencing is analogous to PendingPreparedInstall.
- adopt_staged_declaration_in and the checked production path share the ungated
  commit helper above. A staged open artifact may not bypass the final gate.
  Public commit_declaration_receipt_in (`persistent.rs:3918`; ResidentSession
  delegate `resident.rs:4309`) also bypasses commit_declarations_in's claim: the
  durable branch uses stage/adopt, while the unattached branch calls the lower
  SessionLib compiler/commit directly and changes bindings/visibility.
- Ordinary define/commit_declarations_in (`3890`) and retract_heads_in (`2276`)
  ALREADY earn source admission. Preserve these existing owners and consolidate
  at actual split producer/commit consumers; do not scatter frontend prechecks
  or equate rendering with publication. Caller search found legacy render/validate
  uses mostly component fixtures, but checked declaration adoption above supplies
  the required real actor route.

Parent acknowledged the source-owner category, and leaf acknowledged migrating
checked adoption through the actual source transaction BEFORE reservation plus
shared render/stage/adopt hooks. The exact implementation remains unreviewed.
These concrete owning consumers add session/mod.rs and turn.rs where needed to
the handoff's enumerated runtime cone; parent was notified to coordinate those
same-category paths. No broad registration or R2/R3 work is authorized.

Further public fresh compiler producers, with actor wrappers protected:
turn::compile_activation_renderer (`turn.rs:2699`) requires a native transaction
origin through PreparedRenderer::prepare, but inner specialization (`2728`)
creates/writes scratch template, selects/executes GHC and prepares native images
without earning the real preview admission's source claim. A real preview
admission retained from open A, then fence A, then call this PUBLIC producer under
the existing raw settle_compiler_transaction: native origin exists, but is
identity evidence rather than source permission. Actor's actual caller
(`resident_workbench.rs:7465`) owns an eager SourceDependency ticket, so this is
not a second unguarded actor path. Similarly public
prepare_cell_program_admitted (`turn.rs:3041`) and compile_cell_program_admitted
(`2845`) consume retained RuntimeCellAdmission. Their inner preparation (`3210`)
checks origin/purpose/parser facts and then writes scratch/executes the compiler,
without fresh source admission; actual actor caller (`resident_workbench.rs:8056`)
is protected by its eager source ticket. Issuing RuntimeCellAdmission originally
was gated, but does not retain an affine source claim through later compilation.
The final public owner must cover these compositional producer fronts or make
private producers consume an actual opaque owner capability. A raw origin or
caller-supplied permissive hook cannot suffice. Do not confuse this classification
with standalone original-code preparation or merely observing immutable facts.

Approved phase split rechecked against actual constructors (no hard strategy
inability found): host ChildSessionFactory (`exomonad/host/src/actor_host.rs:3518`)
opens/attaches declaration state and returns a fresh UNBOOTSTRAPPED machine.
The child authored/shared-driver bootstrap is later in
provision_child_session (`resident_workbench.rs:5079`). Thus the actual factory
can run ONCE under its preparation ticket and collect its real close before
mint/adopt final S and enter runtime-owned bootstrap. Current CompilerWorkTicket::run
(`termination.rs:430`) discards settled close by returning only .action; using
that unchanged would advance on Ok(machine) plus Unconfirmed END. Use the
existing actual run_with_close result and inspect authoritative close; unknown
must not adopt, execute bootstrap or publish the machine registry. A borrowed
inner NotStarted is not a substitute for outer close. Preserve primary factory
failure separately from cleanup uncertainty. No factory replay is needed.

Program root preparation (`resident_workbench.rs:12642`) already owns a live
session and mints final S immediately before the authored root entry; adopt at
that actual issuer and execute the entry ONCE under the runtime owner. Do not
run it as preparation and then replay it in a source phase. Workbench root
provisioning has no authored entry before boot. Child remint uses parent context
(`10055`), then await_launch resolves checkpoint/fallback/new-machine scope before
transfer_mailbox_value (`child_launch.rs:532`). Same-session rehome is the first
own-child World with_machine at that stage; adopt final S before that transfer.
Cross-session import uses raw with_host_machine and cannot infer source authority
from placement. Descriptor World and directory reservation exist earlier while
scope is provisional/ROOT; neither may trigger automatic canonical adoption.

Concrete recovery hazard remains important: runner
transfer_recovered_root_public_owner (`resident_workbench.rs:9412`) ITSELF enters
with_machine with the root's own BootstrapWorld BEFORE performing the public
owner epoch transfer. Generic checkout auto-adoption would therefore retain a
stale canonical lease. Host transfer (`actor_host.rs:2158`), confirmed readiness/
finish_binding, then release_root_startup (`2213`) provide the actual post-transfer,
pre-authored interval. Keep pre-transfer checkout as private observation/placement
context with no source permission; explicitly attach the final lease afterward.
Fresh scope identity is not an authorization factory.

Transferred callbacks retain source A while executing in recipient B. The final
owner validates the strong lease's ORIGINAL owner/epoch and separately validates
recipient physical custody; it may not require equality with B's admission owner
or current actor generation. Strong source lease custody survives old generation
wrapper drop, from_context with absent snapshot, generation-None SourceCallback,
and startup-to-behavior handoff. Open is checked lazily at actual source work;
fenced/retired source plus live physical custody still supports neutral progress.
The first-publication canonical C fence remains closed through visibility/ack
without any self-C publisher claim; metadata observation must not earn a compiler
claim or recreate a gate.

Test/oracle truth at this base: migrated public facade tests use real SessionLib,
retained notebook lease and public producers, so they are honest issuer tests;
they must now enter the final actual runtime transaction owner. Current joined
Sleep→bind test uses the real old actor wrapper but manually constructs a
BootstrapWorld separately from its scope and injects aggregate Unconfirmed after
resume. It does not establish canonical WorldC/same-lower-C issuance, physical
END loss or final runtime collector custody. New Generation/root/typed bootstrap
controls must obtain context/world/strong lease through the production issuer
(or the same private production constructor), invoke the actual runtime owner
ONCE, observe its actual earned lower claim after hidden binding and before its
outer close, then verify retention on Unknown/panic/abandon. Keep aggregate-injected
Unknown labelled separately from actual post-resume physical GHC END uncertainty.
Pure continuation and empty/unchanged selection must leave the actual collector
empty; refusal must precede files/compiler/registry/commit effects. Unknown
factory preparation must prevent the second phase without replay.

Disposition: bounded category/contract source search complete. Confirmed checked
declaration actor bypass and public split/compiler analogues were delivered to
parent and leaf while implementation proceeds. C6 phase split is feasible at the
actual production issuers subject to its approved close/custody ordering; no new
registry, authority factory, framework or actor/runtime dependency inversion is
needed. No WIP was approved. Only this authorized report changed and source-only
inspection is not native evidence. Next event/producer: admission leaf supplies
an exact frozen coherent C6 candidate and owning early compile receipt; parent
reactivates full exact-candidate/history review. Parent owns current source-frozen
native gate and its terminal evidence. Yield without periodic polling.


Campaign terminal scope instruction received: the exact turn-producer cone is
accepted and this category search is now CLOSED for this activation. Freeze a
coherent implementation checkpoint before further analogue expansion. Explicit
settlement distinction: deferred Cranelift compilation is synchronous native
CPU work with its own operation completion, not a GHC daemon transaction END.
Its original snapshot checkout's END cannot authorize or settle a later compile.
Earn/retain the source claim around that actual deferred operation and refuse
before fresh compile/cache publication after fencing. GHC renderer/cell/facade
producers instead retain their claims through the actual owning GHC physical
END/abandon. Do not fabricate GHC uncertainty for a synchronous Cranelift-only
control. Confirmed findings, protected actor exceptions, pure observations and
untested/native boundaries above remain separate. Await exact candidate plus
early owning compile; no further category search or source change this activation.


### Exact C6 core owned-history review: 51900a30df plus a82c900783

Task/base/candidate: authority_review; originalbase
5b74c2f1fe010c5c343e0c6f331d54328fbd5090; corebase
345194431d9f8407a7b7e7c4c273eafbce90bfc0; committed core
51900a30df171a3920584eeaa12be0180d6c7bb4; fixture followup
a82c900783e13ee5bf8c107df7ab00ab737437ea. Read exact committed delta (14 files),
actual direct runtime/actor/host consumers, prior cumulative source review, C6
923 decision, and leaf evidence
`/srv/swarm/evidence/inanna/persona-1895-launch-20261019/compiler-failure/reload-admission-20261019/c6-runtime-owner-core/HANDOFF.md`.
The leaf receipt explicitly marks incremental core, native compile/test 0/0 and
unaccepted successors. No WIP was reviewed as candidate; no source edit/build/test
was performed here. Cumulative originalbase..a82 git diff --check passed.

Verdict: CHANGES REQUESTED; safe to obtain EARLY COMPILER FEEDBACK under parent's
explicit admission, with this parcel review-blocked. Not a full C6/R1 acceptance,
not a lifecycle acceptance, and not an acceptance fold. Stop source growth for
first coherent production/cfg(test) compile, then repair the exact blockers.
The trusted build is not deployment. Earlier345/f147 foundation counts are not
candidate519/a82 evidence.

#### Confirmed core blockers

1. Pending-to-Ready loses ORIGINAL source authority.

PendingPreparedInstall now owns the exact source Arc A, and its off-checkout
compiler entry gates A. However compile_off_checkout (`resident.rs:2728`) returns
ReadyPreparedInstall containing only ReadyPreparedSource plus metadata, without
retaining A. Final revalidate_and_run_prepared checks the CURRENT transaction
source and the capsule target scope, not the source that issued its pending
compile. Supported public history: session B snapshots a real turn under source
A's actual transaction, compiles while A is open, then fences A. It can enter a
new unrelated valid B source transaction and install that same Ready capsule into
its original target B. The final checks see B/B, so they no longer consult A.
The actual actor begin_ready_block_split preserves its original context and
therefore protects that actor route; the public owning runtime boundary cannot
require callers to remember the erased original source. This is a surviving
core issuer-to-consumer gap, not a detached-validation successor.

Required repair/control: carry the exact original strong A lease through Ready,
validate its issuing epoch/identity and earn its gate at the actual final install
alongside every applicable target. Use one shared exact-lease admission owner.
Two-real-source public control: snapshot/compile under A, fence A, attempt final
install under open B, require typed A refusal before metadata/native installation.
Keep an open positive and source-neutral already-installed continuation.

2. Earned target gates do not retain target lexical custody through uncertainty.

RuntimeSourceCompilerTransaction::admit stores `(RuntimeSourceAdmission,
RuntimeSourceClaim)` for original A and optional target B. settle on unknown
retains the strong lease of source A only (`source_admission.rs:535–545`), then
moves every gate claim to unconfirmed. It has no strong target B lease to retain.
Concrete lower PUBLIC owning path: create actual sessions/scopes A and B with a
real mutable native binding in B; enter B.with_source_compiler_transaction(A),
mutate canonical target B and observe Unknown. Drop the caller's final B lease.
RuntimeLexicalScopeLease::drop queues B, and B's next reap_admission_leases
(`admission.rs:2299`) retires B/removes its record/binding/native roots while B's
gate retains only a numeric unconfirmed count. Thus the new supported original-A/
recipient-B boundary does not own all of its admitted physical scope custody.
No forged capability or parent-enclosure inference is needed for this public
counterexample.

Actual transferred callback code supplies the same mixed original-A/recipient-B
context: replacement.rs:520 preserves original dependency; delivery uses it in
map_source/run_receiver. A current actor generation may additionally retain B
in some actor histories. This review does NOT claim last-lease drop is already
proven in that particular actor lifecycle; the public runtime contract cannot
rely on an unrequired enclosing actor owner.

Required repair/control: the single transaction owner admits and retains exact
opaque leases, including the target's real canonical lease when different, not
only gate Arcs. Preserve each through actual END/abandon and transfer uncertain
holds to its actual owning lexical record/terminal owner. Two-real-session public
binding control with Unknown, last B lease drop and next reap must show B remains
owned/pending until explicit terminal cleanup. Do not mint a new gate, infer it
from scope names, or add a broader registry.

3. Gate-only static compositional admission omits the supplied admission's
original lease validation.

Public turn::prepare_cell_program_admitted and compile_activation_renderer pass
`admission.source_lease().source_admission()` to owner.admit. owner.admit validates
ONLY its own source lease's epoch/identity, not the supplied admission lease A.
Therefore a valid unrelated source B token can test gate A without validating
A's original issuer. The convenience cell compiler uses A as its source and
validates it, so this omission is specific to the explicit compositional fronts.
Runtime public-owner transfer advances RuntimeAdmissionOwner epoch
(`persistent.rs:3075–3087`; `admission.rs:1748`) and does not retire/fence every
old gate. The source-backed static mismatch is: source B remains valid, A lease
is stale by owner epoch while gate A is still Open; B.admit(gateA) has no A-lease
check, and cell preparation then writes scratch/starts GHC after its purpose/
parser checks. Renderer has the analogous producer ordering.

Certainty limit: missing validation is confirmed directly in the new API; epoch
transfer's invalidation semantics are directly evidenced. This review did NOT
execute a complete retained RuntimeCellAdmission-A plus actual durable-transfer
history, nor prove every recovery precondition of that combined history. Treat
the full stale-epoch compiler counterexample as SOURCE-BACKED / UNEXECUTED, with
that production setup still requiring its real transfer control. Do not label it
an executed GHC bypass or waive the exact original epoch check. The same shared
exact-lease admission/retention repair as blocker2 closes this boundary without
an extra authority factory. Positive same-owner and A-to-B tests plus actual
owner-transfer refusal before scratch/worker submission discriminate it.

4. The new unknown-custody metadata cycle has no ordinary drop terminal owner.

The candidate intentionally inserts an Arc<RuntimeLexicalScopeLease> into
RuntimeAdmissionOwner.lexical_leases[scope].unconfirmed_source_custody. Lease
contains an Arc back to RuntimeAdmissionOwner. This is a real self-cycle.
Confirmed explicit breaker: PersistentSession::retire_scope calls
retire_source_admission (`admission.rs:2105`), removing the actual record, which
drops the retained Arc and eventually queues ordinary lease retirement. A new
recovered context does not clear the old record. There is NO candidate
PersistentSession/RuntimeAdmissionOwner Drop that removes the records. Dropping
the enclosing session/host's ordinary Arc therefore CANNOT destroy that cycle.
The original handoff's enclosing-teardown release implication was withdrawn by
leaf after this finding.

Also distinguish metadata from native machine lifetime: RuntimeAdmissionOwner
contains UUID/epoch, retired scope IDs and lexical records, not the Session,
PreparedEngine, bindings or machine registry. Its self-cycle alone does not
retain the physical machine after session destruction. It does prevent automatic
last-lease drop retirement while the session remains live, then can leak the
metadata owner/scopes/gates indefinitely after ordinary session drop. Unknown
truth must remain honest, but this is not proof of confirmed resource retention
or a real host cleanup release owner.

Required resolution/control: wire the actual existing terminal owner/confirmed
scope retirement to remove every held record and show its resulting lifetime,
or explicitly accept the bounded limitation at campaign ownership while keeping
lifecycle acceptance withheld. Ordinary Arc drop is not a release mechanism.
A Weak owner/scope witness after actual session teardown distinguishes the
cycle leak from a deliberately retained live session. Never clear an uncertain
hold merely because a waiter/PID disappeared. Parent permits early compilation
with this lifecycle piece unaccepted, not final acceptance.

#### Source findings repaired / unproven questions kept separate

The source test runtime_source_owner_keeps_binding_claim_through_close_and_unwind
at519 incorrectly expected pending_claims()==0 after Unknown/panic, while
pending_claims counts active+unconfirmed. Exact fixture-only a82 changes expected
total to1 and separately asserts unconfirmed1. Read the exact a82 delta: no
product retention was weakened, no native result claimed. This source fixture
finding is repaired.

Bootstrap claim/registration helpers in generation_admission.rs still distinguish
World state/external prerequisite from a canonical own C gate: registration_dependency
and with_open_admission under Bootstrap do not themselves consult exposed C.
This review did not finish a new actual production reservation-after-own-C-only
closure history; do not promote that observation to an executed registration
bypass. Required later same-C controls must cover direct public C uncertainty
as well as actor hook state. Broad R1 registrations/R2 remain held.

Runner recovery transfer now calls adopt_final_scope after an Ok
DurablePublicOwnerPublication enum without matching its committed/refused/
recovery-required disposition. The host checks the returned disposition and
refuses startup on noncommitted outcomes, so no resulting authored-progress
bypass was proved here. Preserve this concrete ordering question for the repair
review: approved contract permits adoption after actual successful owner epoch
transfer, not merely Result::Ok. Do not infer a successful transfer from an enum
containing refusal/uncertainty. Source freeze stops further analogue expansion.

#### Positive core structure and scope of review

One PRIVATE runtime runner calls the existing actual
settle_compiler_transaction_cancellable_for_workload. The public capability has
private fields/no constructor/Clone; public off-checkout action gets only a
borrowed reference. Native-origin and closed checks prevent normal use under a
foreign scope or after END. The session entries install a private Arc context,
catch unwind, restore the prior context BEFORE rethrowing, and the outer runner
retains its independent Arc until physical settle. There is no observed public
Arc token escape; original-lease access can only start a NEW checked transaction.
Nested owning entry rejects before action with ForeignTransaction; borrowed
inner producer NotStarted does not settle the outer collector. Owner.validate
checks the ORIGINAL issuer, independently of recipient session. Existing
public None/setter/hook-transfer stack was removed rather than maintained as a
compatibility authority route. Supplemental hook grants no missing lease.
These are substantial correct boundary changes, but do not excuse blockers1–4.

The runner holds claims until its actual close callback; on abnormal unwind it
uses returned=false even if no GHC request was started, retaining uncertainty
conservatively. Actor delegates once rather than nesting ticket.run, and its
same-ticket supplemental world hook accounts bootstrap separately. Pure entry
validates custody without eagerly requiring an Open gate; actual mutation and
immutable facade producers earn lazy claims. Empty/unchanged facade paths remain
claim-free; private fixture callbacks were replaced by the public owner. Ready
actual-scope gate, public mount before compile/stub write, shared checked-declaration
commit hook, and checked-item actor admission before reservation are present.
The latter fixes the confirmed actor mutation bypass by routing reservation and
adoption through actual runtime phases. No second policy registry was added.

Final issuers: explicit child adoption occurs after final remint/fallback/new-machine
resolution and before mailbox transfer; generic observation checkout does not
adopt provisional ROOT. Program-root mint/adopt precedes one authored execution
and returns the same dependency into its descriptor. Factory setup uses actual
run_with_close, checks resources_retired before final adoption/bootstrap/registry
publication, and does not replay. Recovery has a post-transfer insertion point
subject to disposition question above. World/GenerationDependency now strongly
retain the exact lease without ActorGeneration back-reference. First publication
fences the same C before IO and holds its fence through visibility/ack without
claiming C itself. Replacement uses external parent A, not old own C. The remaining
same-C direct-public uncertainty and failed-start cleanup histories need execution.

Pending Cranelift now owns a separate synchronous source compiler phase; its old
snapshot checkout's close no longer masquerades as settlement of deferred work.
It currently gates even cached-image reuse before calling snapshot compile;
no claim is made that a cache-only metadata branch is source-neutral. Final
installation is always fresh authority, and must retain original A as blocker1.
Cranelift-only operation completion is not a physical GHC END fault. GHC cell,
renderer and facade producers instead require actual outer physical END custody.

Test coverage remains partial/unexecuted: public facade positive/narrowed/retired/
pure producers enter the actual runner; mutable binding test observes an actual
earned claim and injects aggregate Unknown or panic. Joined hidden Generation/
root/typed-bootstrap controls use the actor wrapper with actual canonical lease
attachment, but synthetic aggregate Unknown remains distinct from real GHC END
loss. Missing real factory-unknown→no-advance, A-to-B last-target-drop, actual stale
epoch and actual physical compiler fault controls are NOT supplied by a source
claim or naming. Component/integration fixtures still lacking an owner are
explicit unaccepted migration successors, not evidence that MissingTransaction
should be loosened. Detached DeclarationCandidateRender validation still lacks
its full original-source owner and remains a declared unaccepted successor.

Terminal disposition/next action: findings delivered directly to leaf and parent.
Core519+a82 is review-blocked but suitable for parent's explicitly authorized
first owning compile feedback before further source growth. Parent will integrate
for compile-only, retain the observer/terminal receipt, then authorize bounded
repairs of this exact blocker set and remaining declared successors. No acceptance
fold, feature acceptance, inherited foundation counts or deployment is authorized
by this source receipt. Next independent review event is an exact repaired frozen
candidate plus the actual early compile receipt. Only authorized REVIEW-BASE.md
changed. Yield without polling or new category expansion.


Architecture steering received before terminal: read
`/srv/swarm/checkouts/tidepool-compiler-world-plan-20261019/plans/shared-warm-compiler/RUST-CLIENT-MIGRATION.md`
from observed plan checkout HEAD24ec0b506a8bc80d9d06851e197ab8be493162dc (the named
file had no dirty status at read). The required common published-session
compiler client covers queries, ordinary cells, successors, fork/reload/recovery;
every actual operation and warm hit remains under the SAME fresh source owner,
END/cancellation and stale-publication checks. No machine checkout while waiting
and no actor-current reconstruction registry. C6 exact-lease capability must
compose off-checkout with that common client; query-only/legacy adapter routes
cannot become acceptance. This does not expand the frozen519+a82 review or delay
its explicitly permitted early compile. Alpha permits removing confused public
APIs/serialized compatibility with explicit version refusal; old authority is
never upgraded. Baseline quoted/nativegroup failures remain independent.
