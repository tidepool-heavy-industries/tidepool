# A — Close acquired compiler support once, before consumers can use it

Start from the pack's source checkpoint. Own
tidepool/toolchain/src/artifact_inventory/compiler_projection.rs, its Request
construction boundary and focused component tests. Coordinate certified_products
edits with B; do not broaden this into a new inventory framework.

## Why this repair

At the reviewed source, refreshed_native_facts merges completion custody and
then pairs selected rows and native availability to final acquired entries.
with_native_completion_refs merges custody and returns without that pairing.
Its direct consumers include finite target accumulation, retained completions,
World completion advance and snapshot restoration. Request enrichment can then
pair request metadata while its source_support still contains stale projections.

Custody merging can legitimately preserve an old outer ArtifactEntry wrapper
or create a third one. Outer Arc identity is not native authority. Exact full
bindings and immutable OriginalNativeWitness identity establish authority;
paired projections must nevertheless reference their final acquired owner.

## Approved shape

Separate assembly from closure inside the existing owner:

1. Collect the actual roles, selected bindings, completion custody and issued
   facts into private draft state. Perform all custody merges here.
2. One consuming private finalizer pairs selected rows and availability against
   the final acquired entries and maintains the native fact owner. It must not
   merge the same custody again.
3. Only this boundary constructs externally usable CompilerSourceSupport.
   A closed clone is fine; a mutation must return through the finalizer. No
   public refresh call, optional finalized flag, or caller checklist.
4. One private Request constructor consumes closed support and pairs request
   metadata/selection against it. Preserve that request's advertised subset;
   do not widen it to everything retained or available.
5. Migrate new construction, completion-only enrichment, rebuild/narrowing,
   Original-output construction, World advance and snapshot restore. Delete
   duplicate refresh chains and obsolete ways to return a half-closed owner.

Keep existing issued edges, exact historical choices, Empty/Partial/Full
availability, stage ceilings, cancellation checkpoints and typed refusals.
No global interning, payload-only binding equality, latest-module lookup,
permission from custody, or relaxed pointer/witness guard is an acceptable fix.

## Diagnose the real notebook refusal

All four morning notebook cases refuse main:Tidepool.Prelude at their initial
public read. ArtifactIdentityMismatch has several producers; the receipt does
not identify which fired. Trace the actual failed pair through the same issued
inputs. Retain expected/actual binding, canonical/Original identities and the
specific rejecting predicate in bounded typed diagnostics where necessary.
Do not classify by rendered error text or dump the whole inventory.

If the closed-acquisition repair does not explain that actual counterexample,
report a separate finding and fix only a demonstrated violation of the existing
contract. Do not declare correlation a diagnosis or weaken the refusal to get
the rest of the test to run.

The [cause analysis](06-cause-analysis.md#prelude-refusal-diagnostic-decision-tree)
identifies a particularly important discriminator: checked_native_input_decision
compares artifact IDs/roles, not outer Arc pointer equality. Re-pairing a wrapper
without changing those facts cannot by itself explain an ID mismatch. Historical
dependency filtering through a current owner-keyed projection is another lead,
not an established cause of this run.

## Acceptance

- Production-created completion enrichment that actually creates a third outer
  wrapper, immediately followed by demanded projection through the returned
  support; no extra caller refresh.
- The same issued facts through snapshot writer/reader, World advance and each
  Request transition. Consume the transformed result, not its unchanged parent.
- Preserve older snapshots, request subsets and historical issued children.
  Repeated identical enrichment must remain valid without duplicate re-solving.
- Different native witnesses, wrong bindings, unselected roots and conflicting
  actual selections still refuse for the intended typed reason.
- Run the integrated f5eb17 fixture with its genuine Parent7→Child9 recovery
  set. Never fabricate a missing child or bypass admission to satisfy setup.
- Compile owning and affected test consumers; execute a representative notebook
  case early, then all four on the integrated source. Preserve full finite and
  cold histories after A/B join.

Reviewer checks every support-returning route, not only the new helper. Prefer
one private consuming draft/finalizer to a generic typestate framework. If closure
would need to infer a missing grant, stop and identify its actual issuer.
