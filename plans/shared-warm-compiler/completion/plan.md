# Complete the compiler-native architecture

Status: implementation resumed after architectural review on 2026-10-20.
Controlling branch: `integration/compiler-query-warm-final`.
Reviewed root source: `784fd9fc1fec805793bc178cd4751062b0279838`.
Current implementation contract: [g2-completion.md](g2-completion.md).
It owns this wave's work split, constructor boundaries, sequencing and acceptance.
[ownership-completion.md](ownership-completion.md) and
[package-runtime-join.md](package-runtime-join.md) supply supporting contracts.
[contracts.md](contracts.md) remains the shared semantic contract except where
the current implementation contract explicitly refines it.
Workers may replace unfinished scaffolding; preserving its shape is not a goal.

## Direction

Haskell owns the live checked compiler environment. Rust owns published runtime
facts, grants, exact execution provenance, resources and publication. Warm work
carries compiler-owned facts; cold recovery reconstructs the same logical world.
The baseline already has persistent compiler processes. This wave removes repeated
reconstruction and competing execution routes, not a process-per-cell design.

There is one resident compiler route, one physical-output issuer, one native
planner and one publication owner. Reuse the existing parser, transactions/END,
certifier, evaluator, journals, and test infrastructure. Delete replaced routes.

## Current evidence and disposition

The reviewed checkpoint is clean; G2 and release qualification remain incomplete.

- Package-interface custody, parsed HOME ownership and schema-5 catalog controls
  have a reviewed joined candidate at `c75b761909`; its four HOME/interop controls
  and 34 Python controls passed. It diverges from canonical: integrate its reviewed
  changes without losing later CLI, runtime and retained-descriptor repairs.
- New cohort availability must be issued once, independently of actual stage
  demand. Current normalization still re-solves and assumes Full. Nominal exact
  references, stage gates and cold custody must join before World acceptance.
- Closed native installation is implemented. Captured values, host mounts and
  Ready renderers must use the same immutable plan and per-install binding owner;
  their older code-export route remains a required deletion.
- Publication ownership is implemented. Unknown-source reporting still invents
  evidence, and confirmed-publication caller loss needs a real hosted control.
- Native test modes and declared service resources have diverged. Repair the
  generator/runner boundary, then qualify the actual schema-5 catalog and bundle.
- Keyed completion collection and the stronger historical-link/cold controls
  remain reviewed source candidates awaiting joined compilation and execution.

Earlier leaf counts qualify only their exact code and boundary. Preserve the
historical failure ledger and useful test contracts; do not delete cases merely
because their old setup used a removed API.

## Ownership and integration

| Area | Current owner | Result |
| --- | --- | --- |
| Native producer | engine_completion / home_certificate_flow | Custody/Home integration, warm joined compilation and actual catalog |
| Native selection | original_reference_boundary / stage_input_design | Once-only issuance, exact choices, nominal custody and stage execution |
| Artifact storage | audit_native_selection | Finish reviewed keyed completion collection |
| Codegen custody | review_world_lifetime_architecture | Exact HOME/package instance keys, leases and attachments |
| Runtime join | compiler_demand_transport | Common open/closed recipe, host/Ready callers and fallback deletion |
| Lifecycle qualification | audit_reload_ownership | Truthful reporting and real default-reload/caller-loss controls |
| Wire cutover | common_native_output | Delete cache-only variants with their owning runtime/compiler consumers |
| Joined histories | closed_recipe_pairing / package_envelope_identity | Ordinary/discovery/END and exact historical parent-only recovery |
| Build/test resources | native_build_service | Mode/resource coupling and isolated owned-service catalog gate |
| Map | compiler_map_concision | Source and measured work against the actual joined checkpoint |
| Integration | root | Shared contracts, independent review, early joined tests, map and release evidence |

Reuse existing isolated warm checkouts. Children report proposal, blocker,
review-ready and settled events; parents continue the ready frontier. Each area
consolidates reviewed commits, records source/evidence and sends exact OIDs.
No native Codex goals. No unrelated task merely to keep an agent busy.

Root reviews production consumers and failure paths directly. Independent review
checks each joined area for bypasses, repeated work and coupled fixtures. Rebase
on received contracts; do not silently resolve an authority conflict. Publish
integration checkpoints labelled with their actual qualification state.

## Sequence and gates

1. Freeze the G2 owner/recipe/lease/successor signatures on this checkpoint.
2. Join compiler and runtime tranches incrementally from reviewed exact commits.
3. Execute startup through two package-using public-B cells early, then complete
   the production histories and remaining ordinary-workflow controls.
4. Freeze one matched bundle and deliver the next Persona candidate after G2.
5. Complete G3 and reconcile/push reviewed release/main checkpoints.

### G2: early Persona candidate

One exact frozen bundle must demonstrate:

- Actual hosted Haskell tool with ordinary bindings, declarations, explicit/empty
  imports, expressions, mixed cells, source-defined types and later reuse.
- Actual used/unused private inputs, crossed/retired refusals and compile-before-
  effects. Uncertain END cannot execute or publish.
- All five discovery verbs assert semantic fields and use the held compiler;
  read-only queries do not mutate public state and pinned mismatches refuse.
- Successors survive predecessor realization eviction and disposable-output
  removal while required immutable custody remains retained.
- Actual default reload success/refusal and caller-loss retention; stale source
  and cancellation preserve truthful public state and effect evidence.
- Selected Persona spec, tools and initial memory/context verified, followed by
  a bounded real DeepSeek multi-turn smoke on the same bundle.

Force may explicitly refuse at G2; this is not campaign completion. No known bug
in the advertised ordinary workflow is waved through as a legacy-path pass.
Launch fresh conversation state while preserving authored files/durable memory.

### G3: campaign completion

Finish force, callback/continuation capture, forks, cold recovery, worker loss,
exact historical bindings and cleanup uncertainty. Close every recorded failure
with repair or an obsolete-test disposition whose useful contract survives.
Run affected suites, compiler fixtures and bundle-owned M1-M3 plus Persona
histories. Record exact source, artifact, command, executed count and outcome.

Update `docs/compiler-work-map.md` against the shipped source and execution.
Measure packaged warm/cold costs and counts: hydration, physical admission,
dependency planning, products and retained memory. Structural savings and measured
speedup are separate claims. Final delivery includes reconciled pushed main.

## Resource and handoff rules

At implementation resumption: scratch 172 GiB free with 100 GiB floor;
available RAM 105 GiB and build.slice approximately 32 GiB, no pressure.
Use measured output growth and existing warm outputs, not simultaneous cold
expansion. Follow admitted pinned local-only commands and current server policy.
Preserve evidence/WIP; no broad GC. Stop only owned idle daemons deliberately.
If evidence requires a genuinely new architecture decision, retain the smallest
counterexample and alternatives for root; continue independent work, then hand
over cleanly instead of adding a fallback.
