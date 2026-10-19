# Reload authority review at the campaign base

Reviewer: `/root/warm_compiler_campaign/reload/authority_review`.

Current cumulative source review: campaign base
`5b74c2f1fe010c5c343e0c6f331d54328fbd5090` through exact repair candidate
`9bb4c1eb4bf5a0775dae68beb119446bb34d06b9`, read from committed objects in
`/srv/swarm/checkouts/tidepool-shared-warm-reload-admission-20261019`.
Disposition: BLOCKED on bootstrap continuation/native-END custody and the
immutable actor-start facade compiler claim that drops before outer END,
specified in the final checkpoint and adjacent producer audit below. The fe3
runtime generation-claim loss, ownership enum and fixture lease findings have
source repairs in this candidate. No source acceptance or integration readiness
is issued. Compile/test execution remains 0: root's shared native lane is held
elsewhere. Cumulative whitespace check passes. No build, cleanup experiment,
product source edit or release qualification occurred.

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
