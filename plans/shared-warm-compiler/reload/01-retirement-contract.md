# 01 — Generation admission and retained execution

Baseline: `70a92c65a02a591f76590daf4e5bfa3e6fcbf723`. This child defines the shared
contract before shutdown mechanics or API migration. Its owner is the actor/runtime
boundary implementer; its reviewer independently follows stale handles and
source-bearing callback transfers through real consumers. Integration belongs to
the reload supervisor. No implementation or test execution is recorded here.

## Current issuance and the missing distinction

`ActorGeneration` retains installation, source/context, public owner and lexical
scope. `ActorRequestSnapshot::select` retains that complete generation through
dispatch. `InstalledToolsState` owns the current selection. New replacement
generation preparation already calls `mint_isolated_scope_with_lease` and clears
notebook imports in `prepare_actor_generation`.

These are in [resident_workbench.rs](../../../exomonad/actor/src/resident_workbench.rs),
especially `ActorGeneration`, `ActorRequestSnapshot`, `InstalledToolsState`,
`ActorGenerationPublisher::publish` and `prepare_actor_generation`. The baseline
retained snapshot authorizes late new work. Replace that implicit lifetime-to-
admission relationship with an explicit owner-issued admission state while
preserving the immutable snapshot for already admitted code and observations.

An additional bypass is concrete: [request/sources.rs](../../../exomonad/actor/src/request/sources.rs)
`CallbackExecution::transferred` changes the captured snapshot into
`ActorRequestSnapshot::SourceCallback` while retaining its source and old lexical
scope. `generation()` returns `None` for that variant. Checking only that accessor
would let source callbacks bypass retirement. `CallbackExecution::from_context`
and direct source startup constructors also need explicit provenance; absence of
an optional generation field cannot certify independence.

## Contract and private type shapes

Extend the existing generation/context and resource owners. The following are
design roles, not a new public framework or compiled type declarations:

| Role | Issuer / payload | Rule |
|---|---|---|
| Generation dependency | Exact actor incarnation, notebook generation/public owner, and immutable source/world owner from the actual installation/capture issuer | Labels, source hashes or current module names cannot issue it. Shared source bytes do not imply shared retirement domain. |
| Independent-world evidence | Existing child/fork/source-neutral owner proves its resource has an independent selected world or no executable source dependency | An LLM actor is independent because its actual world owner says so, not because no provider is attached at inspection time. |
| Generation admission | Existing generation owner holds Open, Fenced(attempt), or Retired(successor); terminal/unavailable state remains explicit | Borrowed source custody and snapshot/cache hits cannot reopen admission. |
| Admitted continuation custody | Existing invocation, compiler ticket, `ResidentHole`, source leases and native roots | Survives a fence only for the work already admitted; it carries no new source-dependent admission. |
| Source-dependent resource registration | Existing lifecycle/resource issuer records generation dependency with exact resource identity and cleanup owner | Registration and transfer recheck the generation gate; unknown dependency is refusal, never implicit independence. |

There is no global latest-module map, second actor-current registry, duplicate
resource registry or blanket effect-row gate. Owner-issued state can be shared by
handles, but consumers that create authority must check its present admission.
The old selected compiler/world root and historical native graph remain immutable.
Its dependency edges cannot be reissued against the successor merely because ABI
or payload bytes match.

Every request explicitly captures its compilation world. A remains valid for
already admitted work and independent forks even after this actor selects B.
Retained alternative implementations stay in physical custody without becoming
optional roots of B. Follow the handoff's production seed policy and keep strict
validation when an actually demanded historical edge conflicts with explicit
selection. See `for_compiler_roles` in
[declaration_context.rs](../../../tidepool/toolchain/src/declaration_context.rs).

## Self-reload boundary and actual consumers

Keep `CoherentReloadOperation::run`'s Continuing path,
`WorkbenchExecutionControl::complete_continuing_publication` and
`source_effect::resume_source_answer`. Owner publication settles before delivery
of the ordinary Haskell answer; continuation failure cannot erase the committed
reload. The coordinator is exempt from waiting on its own terminal invocation,
not exempt from future source-dependent admission checks. It may evaluate pure
already compiled code; it may not reopen old compilation or resource admission.
No assumption that Haskell stops after returning the reload receipt is valid.

Use operation inputs and issuing owners for the following audit:

| Existing consumer | Required decision after the fence |
|---|---|
| `WorkbenchCompilerService::admit_work` / `CompilerWorkTicket::admit` in [capabilities.rs](../../../exomonad/actor/src/capabilities.rs) and [termination.rs](../../../exomonad/actor/src/termination.rs); runtime compile/install entries | Fresh work derived from A refuses; accepted A tickets close through their original END owner. A retained warm snapshot cannot authorize a new ticket. |
| `ResidentActorStart::capture`/`capture_spawn`, start/replacement and startup source/shutdown installation | New generation-dependent behavior or callback registration refuses. Actual independently captured LLM worlds remain independent; do not stop every descendant. |
| `KernelContext::spawn_resource`/`spawn_worker_scoped`, `LifecycleHandle::resource_admission`, directory registration/transfer | Add dependency admission to these existing issuance boundaries. Actor-incarnation resource admission alone is currently broader than notebook generation. Do not close the LLM actor lifecycle to retire only its notebook generation. |
| `SourceBinding`, `CallbackExecution`, source handoff, display callbacks, forms, watches/routes/reply-tool callbacks | Preserve and check dependency at both registration and activation. Issued handle aliases and transferred `SourceCallback` must not escape. |
| `prepare_actor_display_boundary` and `owned_effect_task` display preparation | Permit already prepared non-callable receipt/text observation where it needs no new A compiler/resource authority. Refuse requested old specialization or new callable expansion/selection registration. A blanket Display ban is unnecessary. |
| `SourceReq` inspection, RuntimeInfo/status, already issued receipt/result observation | Retained read-only source/result evidence can remain observable. No current selection or new execution grant follows from observation. |
| `External`, Command, Sleep, request reply settlement and native IO consumers | Pure execution and truly independent granted IO may continue. Classify concrete operation variants: starting a retained callback/retained binding, installing a type-dependent response or registering a source handler is different from reading text or settling an already admitted result. Preserve existing permissions/cancellation. |

`ResidentActorBoundary` in `resident_workbench.rs`, `owned_effect_task`/
`await_effect` in [owned_workbench.rs](../../../exomonad/actor/src/resident_actor/owned_workbench.rs),
and `prepare_source_effect`/`prepare_reload_effect` in
[source_effect.rs](../../../exomonad/actor/src/resident_actor/source_effect.rs)
are the audit map, not a place to introduce a second effect interpreter.
Deferred external work must not be blanket-certified source-neutral: the owning
handler knows whether its result starts/registers a source-dependent resource.
Carry the existing dependency to that handler and check there.

Already admitted continuations can retain old code and type/dictionary/display
provenance until completion or actual cancellation. Releasing selection does not
release physical bytes. The new invariant is closed new admission, not immediate
zero old allocations. A specialized display that needs compilation may return a
typed retirement refusal while a simple precompiled reload receipt can display.
Pure nontermination uses existing execution budgets/cancellation; reload does not
pretend that the coordinator's physical cleanup is already confirmed.

## Implementation and reviewer waves

| Wave | Worker deliverable | Independent review and evidence |
|---|---|---|
| R1a — Issuance map | Inventory all production generation/snapshot/source/resource constructors and consumers above. Distinguish dependent, independent and unqualified paths; settle private dependency type and generation gate with runtime owner | Reviewer follows old policy, `SourceCallback`, from-context startup and transferred callback to actual next use. Exact source paths and counterexamples accompany any unknown path. No downstream wave assumes unknown means safe. |
| R1b — Gate at authority owners | Add private dependency/gate to existing owning state and checked constructors; enforce new compiler/install/start/register/transfer authority | Compile actor/runtime production **and test** consumers early. Reviewer attempts stale aliases, missing dependency and same-label/different-incarnation bypass; no new public factory used only in tests. |
| R1c — Continuing and independent operations | Preserve old receipt/compiled continuation, independent-world child semantics and neutral existing grants; refuse generation-dependent follow-on actions | Real self-reload history executes pure old closure, static receipt display and neutral read, then attempts fresh compile/new callback/new typed service. Observe exact refusal and unchanged B; old END and receipts remain inspectable. |

Dependencies: consume campaign world identity without merging historical graphs
into compiler namespace. R1b is prerequisite for the fence in child 02. API owner
may prepare schema/docs after R1a but cannot claim user safety before R1b/R1c.

## Tests, deletion and exact evidence

Use a small independent stateful model: generation gate, selected root, admitted
operations and source-bearing registrations. Generate Issue/Admit/Fence/Transfer/
Activate/Complete/Drop histories, including ABA actor incarnation and independently
captured overlapping worlds. Compare to actual production issuers/consumers;
do not create authority by private-field writes. Save pre-transition model state
independently rather than rereading the production snapshot as the oracle.

Required controls: a valid pre-fence path; stale issued policy after B; transferred
`SourceCallback`; callback already queued at the fence; new worker registration;
source-neutral text/receipt after B; old accepted compiler END after B; independent
child world; identical code with different source/root issuers; and actual required
historical native children. These obligations require code execution later; plan
authoring supplies no passing counts.

Delete the private assumption that a retained installation/lexical lease alone
authorizes fresh work after retirement. Remove source-callback constructors that
discard dependency provenance; replace them at the owning issuer, not with a
parallel checked/unchecked public API. Retire only the old *late new admission*
positive assertions, retaining immutable prompt/result/source observation and
independent-world controls. Do not delete historical dependency tests simply
because reload now closes future admission.

Each leaf handoff records base/final OIDs, complete diff, directly affected target
compile, actual exact tests/counts, refusal variants, source/worker/cleanup owner
observations and evidence location. Report any uncompiled callback/host consumer.
Review R1a–R1c cumulatively before merging into the reload area branch.
