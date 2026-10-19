# Warm worker supervisor

The [compiler-owned demand-service amendment](../DEMAND-SERVICE.md) controls
new cross-area interfaces and sequencing. Keep the concrete source/invariant
evidence below, but do not implement a duplicate host compiler world or
require full input export before the service can answer a warm query.

Status: proposed implementation campaign; no implementation or performance qualification is implied. Plan source: `70a92c65a02a591f76590daf4e5bfa3e6fcbf723`. The parent campaign's shared contract owns cross-boundary types and decisions; this subtree specifies the worker realization, pool and frontend work beneath it.

## Outcome and boundaries

A bounded pool of warm GHC processes serves many immutable exact compilation worlds. A world belongs to an issued host snapshot, not to an ActorId or the worker's most recently active namespace. Actors may share a world, fork a base, publish successors, or retain values with historical executable dependencies. Suspension retains no CPU grant, compiler transaction or machine checkout. Each new compile has a fresh purpose, receiving allowance, cancellation/END observation and publication origin.

Deliver three waves:

1. [Exact realizations](01-exact-realizations.md): worker-instance-bound acceleration of a host-issued exact root, with source-free acquisition, immutable selected facts and fresh attempts.
2. [Bounded pool](02-bounded-pool.md): use realization locality within the existing resource/worker owner, with fair bounded scheduling, independent interpreter epochs and measured retention budgets.
3. [Import context experiment](03-import-context-experiment.md): establish whether complete checked-import state can eliminate remaining repeated scope work while preserving ordinary module semantics. Production adoption is conditional on an explicit go/no-go.

This work does not create one GHC process per actor, freely merge divergent raw GHC objects, select implementations from retained inventory, or adopt GHCi evaluation. The confirmed selection repair at this base is a non-negotiable oracle: `RequestNativeAvailability::for_compiler_roles` derives optional roots from canonical affinity **and** explicit Original selection, then follows exact required edges. A retained equal-interface alternative is not another selected root. An excluded optional root can remain an exact required historical child; never rebind that edge to the current module. Keep `validate_consumed_native_namespace` strict.

## Existing owners to extend

| Mechanism | Production owner at plan base |
| --- | --- |
| Resident compiler, completed source/original versions, attempt/executable ownership | `bridge/haskell/src/Tidepool/GhcPipeline.hs`: `CompilerUniverse`, `CompletedModuleVersion`, `withResidentCompilerScopes`, `residentCompileOne`, `activateCompilerInterpreter` |
| Exact physical input admission, captured bytes and interface decode | `ExactScope.hs`: `readExactScopeWithOwner`; `OwnedInputTransport.hs`; `ExactHydration.hs`: `RequestIfaceDecoder` |
| Private home resolution, fixed package owner and lazy type knots | `ExactHydration.hs`: `forkExactContextWithPackageFacts`, `hydrateOriginalInterfaces`, `exactHomeInstancesFor` |
| Typed request/protocol and transport/END | `tidepool/extract-cmd/src/request.rs`, `endpoint.rs`; `Tidepool/ExtractRequest.hs`, `CompilerRequest.hs`, `WorkerServer.hs`, `app/Main.hs` |
| Bounded scheduling, child lifecycle and resource observation | `tidepool/extract-cmd/src/daemon.rs`: `ResourceAdmission`, `serve_workers`, `service_transaction`, `Worker`; `resources.rs` |
| Existing checked query and generated module frontend | `InspectionRunner.hs`, `Introspection.hs`, `GhcPipeline.hs::compileChecked`; runtime `session/inspection.rs`; actor `resident_workbench.rs` |
| Authoritative host root/publication and immutable byte leases | Parent campaign host-input and owned-input tranches; existing `ScopedCompilerContext`, paired publication and exact source/support issuers |

Do not add a second actor-current registry, process launcher, package resolver, native qualifier or byte store. A realization references the selected immutable facts and their actual owners. It must not retain an entire mutable per-actor HscEnv chain or cumulative request directory.

## Source-backed constraints

GHC NameCache interns `(unit,module,occurrence)`, not source versions. Old and new home objects can remain reachable, but their equal names/`eqType` do not establish the same issuer closure. Closed worlds stay distinct; cross-world composition enters existing exact binding admission.

The worker currently permits shared fixed EPS only for CompManager with one definite home unit. Fresh home/file finder cells, empty mutable knots, exact selected HPT and removed request preview plugins are essential. Lazy HMI details retain their original knot/package owner. Copying EPS while retaining those thunks can separate package types from their instances. A new NameCache cannot borrow already decoded types/EPS from another issuer.

Metadata retention and executable residency are separate. Compatible graphs may share the current interpreter; incompatible object/native replacements require the existing epoch retirement/relink path. Plugin process globals and untracked TH need explicit eligibility/refusal and existing no-replay policy. One active outer transaction per worker remains mandatory because CWD, hooks, capture and session state are shared.

Lookup already uses CheckOnly, shares metadata checking within a request and elides unused leaf interfaces. The target opportunity is cross-request exact state and repeated scope assembly, not imaginary lookup code generation. Original input content/decoded interface reuse also already exists.

## Proposed minimal wire decision

The shared contract must adopt or replace this proposal before wave 1 branches. Use one source-free control operation, `EnsureExactInputs`, inside a fresh transaction. It carries the authoritative host exact-root reference and either an existing realization reference or the owned selected-input acquisition. Its closed outcomes are `Ready`, `NeedsAcquisition` and typed `Rejected`. `NeedsAcquisition(root)` requests the complete portable selection for that same root, without requiring an unknown/evicted worker to enumerate missing parts. A checked delta can be offered only against an authenticated retained base; an unavailable base requests complete acquisition. The result is returned before any user-source operation is admitted. Acquiring owned inputs can then produce `Ready` in that transaction. Only after `Ready` does the normal versioned source/query request name the root and realization alongside its current purpose and inputs. BEGIN/slot admission and the source-free exchange may already be accepted; the required proof is that no user-source operation was accepted or executed. A successful miss-to-owned-acquisition continuation is a new source-free exchange, not a retried compile. There is no retry/fallback following accepted uncertainty.

A realization reference binds producer/profile/package owner, fresh worker-instance issuer and opaque selected-root slot. It is not a hash capability or current runtime permission. Daemon boot epoch plus slot is insufficient because a child can rotate in the same slot. The root's issuing owner supplies all selection and graph authority; transport inventories supply bytes only. Semantic registration/retention remains with tidepool-toolchain and the Haskell compiler universe; extract-cmd owns only typed transport, routing and process resources. Ready holds its realization internally through END; the host independently retains portable input owners after END, without a CPU/transaction lease. Version the matched Rust/Haskell envelopes through their existing owner and reject incompatible versions. Direct execution uses the same acquisition semantics without persistent locality.

No new general query language or alternative protocol matrix is planned. If host root authentication, acquisition, or pre-source outcome semantics disagree with this shape, the supervisor records a concrete shared-contract amendment and waits for its integration before dependent implementation.

## Supervisor and wave integration

The warm-workers Sol supervisor remains responsible across its successive waves under the single Sol campaign supervisor and [campaign supervision contract](../SUPERVISION.md). It takes the parent contract, reviewed implementation base, owned paths, resource lane and qualified fixture construction as its initial typed assignment. It branches bounded implementation work from the last integrated wave in separate real checkouts, routes exact-candidate review and repair, and folds each complete base-to-tip result back into its supervisor integration branch. It does not create one short-lived supervisor per file or send unbounded parallel GHC jobs.

Wave 1 depends on host exact-root issuance, owned-inputs/01 admitted row ownership and the existing transport plus the 03 Wave A lease/granularity contract. It does not wait for all of owned-inputs/03: its joined transport Wave B consumes the realized protocol afterward. Package source-backed proof is optional; explicitly scoped observation mode is a valid initial capability when its consumed-byte association remains sound. Wave 2 may implement pure policy/resource helpers alongside wave 1 after their interfaces are fixed, but its end-to-end integration depends on valid realization references. Wave 3 depends on the joined first two waves and a retained baseline. The parent supervisor integrates each accepted wave before later consumers accumulate against an unreviewed surface. Workers own scoped source paths; overlapping protocol/host interfaces have one declared owner and one reviewed amendment.

Each assignment carries exact base, contract revision, owned files, independent semantic oracle, acceptance commands, resource budget and escalation condition. Independent review examines production users, lifetime/refusal paths and the complete joined diff. If a same-name version, lazy instance or accepted-uncertainty counterexample appears, stop the dependent path and repair the owning boundary; do not weaken validation or switch frameworks to hide it.

## Gates, resources and retained evidence

Before implementation, read the required selection handoff and keep its independent root/edge expectations in the new frontdoor histories:
`/srv/swarm/evidence/inanna/persona-1895-launch-20261019/compiler-failure/HLS-REWRITE-HANDOFF.md`.
It records a confirmed seed-policy defect; the associated live Persona cause and stronger hosted result were not established merely by that unit repair. Inspect terminal evidence before updating their status.

Compile each changed owner and its tests/direct consumers through the admitted pinned repository commands. Use existing counted Rust and native Tasty runners; reject zero selection. Run one joined actor history early: startup JSON/shared-root Jev support -> Files/Memory cell -> native lookup -> notebook lookup -> later pure cell. Qualification owns presentation assertions and exact frozen bundle evidence. Include both optional-root winners, genuine required-child conflict refusal, reload, cancellation and cold recovery where touched.

Record source OID, artifact/hash, exact command, selected/executed counts, exit status and retained reports. Distinguish structural counters from wall/CPU measurements. Report unique selected facts and retained backing bytes, not references as unique objects; include process-tree RSS/PSS/SwapPss, page faults, ancestor headroom, allocator/GC, graph/interface expansion and scratch growth. Historical worker timings and two idle memory snapshots do not qualify this source.

Existing 21 GiB pool/7 GiB worker defaults and 1024-request/RSS rotation are operational policies, not physical host limits. Core wave 2 proves bounded realization retention and strong-owner release under existing admission with 1/2/3 workers. A new budget option and larger process/swap sweep are optional post-measurement follow-ups, separately approved only if current policy blocks demonstrated useful parallelism; they do not gate the frontend experiment or change infrastructure while authoring plans. The earlier 8–12-process/64–84 GiB range is an unqualified experiment envelope subject to refreshed actual worker-ancestor limits and other work. An earlier compiler-process sample had a 104 GiB memory/2 GiB swap enclosing limit; that scope-specific observation is not a limit inferred for the plan author or a future launch. A matching exact-root handle is not permission to bypass memory/disk admission.

Every wave deletes superseded policy/routes within its scope and updates nearest owning guidance. Retain the cold production path as a differential oracle only while it has a named obligation; remove experimental-only APIs/tests after the go/no-go. Final acceptance requires joined correctness and measured work removal. Deployment and retirement remain parent release work, with no claim that planning delivered the implementation.
