# Incremental Rust capability rollout

Objective: make orchestration consumers understandable and enforceable through
narrow behavior contracts, with composable implementation dependencies and owned
service contexts. Follow [the shared pattern](../docs/capability-boundaries.md).
This work remains separate from Persona's blocking compiler certification repair.

## First production slice: compiler admission

Owner: the resident actor compiler lifecycle. Isolated implementation branch:
`refactor/compiler-capabilities-20261016`.

- Introduce domain-owned `CompilerAdmission` and `HasCompilerAdmission`.
- Wire `WorkbenchServices` at the existing machine-access composition point.
- Move actual compiler action and publication admission consumers to the service.
- Keep the captured exact `CompilerCloseOwner` explicit per operation.
- Separate observation from cancellation control and keep issued work tickets
  affine. Shared preparation observers cannot cancel their producer.
- Preserve existing owner-specific publication admission policies and the
  distinction between native close and post-close host admission cancellation.
- Remove superseded construction/access paths after callers migrate.

Acceptance: compile affected consumers and test targets; run the actual admission,
partial-registration compensation, abandoned-ticket, publication, shared-producer,
post-close cancellation and lookup/recovery controls. Run the actor notebook
contract cohort. Independent review must verify exact final source and distinguish
private-type compile-fail coverage gaps from executed runtime controls.

The first service has no peer service dependencies. Its closed-service form does
not block a later context-indexed implementation. Do not add a fictional dependency
or generic provider framework to this slice.

## Second slice: durable inbox observation and delivery

Begin after review of the first contract's real consumers. Preserve the existing
single durable owner and delivery supervisor.

1. Consolidate notification authorization/projection through the production owner;
   remove the duplicate test-only policy implementation.
2. Define publisher/observer behavior contracts and explicit delivery permits.
   Do not make observation capable of acknowledging or settling delivery.
3. Trace service dependencies. Where one operation actually composes another
   service, use `Service<Context>` with implementation-only capability bounds.
4. Move blocking durable operations off async runtime workers through the existing
   tracked execution mechanism. Preserve durability-before-transport and cancellation
   ownership; an abandoned waiter must not erase in-flight durable work.
5. Exercise publish, interruption, reopen, delivery reconciliation and shutdown
   through the real owner, with an independent history model where useful.

Do not add a second writer actor, registry or durable format as part of accessor
rollout. Escalate a demonstrated need for those changes separately.

## Shared declaration and wiring machinery

Use the first two production contracts to decide what repeats. Compare explicit
named traits/impls with a small macro that generates only access and wiring.
Require useful missing-capability and wrong-service diagnostics and inspect the
expanded code. Support ordinary owned contexts, existing actor clients, optional
fixed-context service erasure and context-indexed behavior without auto-cloning
operation authority.

Keep handwritten wiring if a macro only saves a few getters while obscuring the
contract. A generic `Has<Key>` layer or external framework needs an actual generic
consumer or construction requirement; do not introduce either preemptively.

## Later candidates and limits

- Authored compiler certification now distinguishes current namespace roles,
  complete closure-owned executable demand, and historical custody. Preserve
  this separation in consumer traits: accept paired `CompilerSourceSupport`
  and issuer-owned `SelectedOriginalClosure` where those are the contract,
  rather than parallel raw inventories and inferred role maps. Exercise fresh
  publication after recovery, published subset policy with complete authored
  demand, and historical same-role bindings that must remain custody.
- Publication continuation fixtures must carry exact prior support through the
  production continuation path. Published source policy now needs an explicit
  immutable selection of the native-offer facts chosen by its issuer. Retain
  references into the existing offer snapshot map, authenticate those facts on
  recovery, and resolve only the recorded references when reopening. A later
  offer for the same module can be relevant without belonging to that original
  publication. Use a domain ID as a locator, never as standalone authority.
  The implementation parcel must record its format-version change and reject
  older snapshots that cannot prove the association; do not infer it from the
  latest inventory or embed a complete context recursively inside its own role.
  Initial issuance, unrelated later availability, same-owner distinct proofs,
  missing originals, and real writer/reader recovery belong in its controls.
- Persistent-policy verification and transient input selection are distinct
  contracts. Narrowing a compiler invocation must start from its selected
  owners and retain their required type closure. Reopening a published policy
  must refuse lost required custody. Do not use a generic-looking projection
  helper for both jobs; keep these operations on their owning types and cover
  excluded owners as well as selected ones, including overlapping dependencies.
- Compiler inventory observations can expose narrow read contracts while exact
  selections remain inventory-issued handles. Do not turn a service trait into
  a universal exemption from recovered-state authentication.
- Context projection and filesystem/source ownership can use the same pattern
  when their accepted owner refactors reach a consumer boundary. Retain proof,
  revision and uncertainty facts in domain types rather than service booleans.
- Effect dispatch generation, logical compiler filenames and workspace fixture
  convergence are independent plans, not prerequisites for capability rollout.

Each parcel records removed access paths, dynamic checks that remain necessary,
executed evidence and joined acceptance. Keep format compatibility decisions
explicit. Do not hold a working Persona candidate for optional architecture work.
