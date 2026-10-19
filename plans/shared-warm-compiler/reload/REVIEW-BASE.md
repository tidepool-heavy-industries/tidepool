# Reload authority review at the campaign base

Reviewer: `/root/warm_compiler_campaign/reload/authority_review`.
Source: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`, branch
`feature/shared-warm/reload`, checkout
`/srv/swarm/checkouts/tidepool-shared-warm-reload-20261019`.
This is source evidence and an independent oracle proposal. No compilation,
test execution, cleanup experiment or release qualification occurred.

The baseline intentionally permits late work from retained A selections. The
paths below are required migration coverage and counterexamples to incomplete
gating implementations, not claims that this baseline violates its old contract.
Shared semantic decisions remain with the campaign coordinator.

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
