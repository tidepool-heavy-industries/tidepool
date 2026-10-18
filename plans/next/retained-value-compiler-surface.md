# Retained value compiler surfaces

Status: implementation plan for review, based on
`dcd107eb0a4677753d9a3ae631b2556f04105c6e`. This is a separate NEXT branch.
Architecture changes, compilation and native qualification are pending. Do not
include this plan's proposed changes in the current release implicitly.

## Result and measured boundary

Remove repeated composition of complete historical captured-value custody from
the compiler offer. Continue offering all interfaces required by the admitted
lexical scope. Keep each capture's historical execution graph with its original
issuer, and select it only for an authenticated use. Preserve compiler-time
source, instance, family, template and native requirements.

The closed packaged Persona2204 history identifies offer construction as the
growing pre-worker cost. Its four accumulation offers take 5.514, 9.464, 13.856
and 18.243 seconds; corresponding physical compiler service remains about
3.09–3.23 seconds. The repeated source-display offer takes 23.432 seconds.
Native-offer-custody child events account for only about 0.844 seconds of that
last offer. These observations identify the broad boundary, not an exclusive
attribution to one graph operation or an estimate of achievable savings.

Retained evidence and exact timing labels live in
`/srv/swarm/evidence/inanna/admission-analysis-review-20261018/persona-history-growth-2204-next/`.
The isolated execution is 579.757931534 seconds, the maintained case timer is
577.903550336 seconds, and the cohort owner is 584.915630342 seconds.

## Existing dataflow

1. `PersistentSession::build_scoped_compile_view` distinguishes visible names
   from scope-reachable modules. `capture_value_interfaces` obtains each
   reachable module's exact `CheckedValueArtifact`. Reachability includes
   historical dependencies; it is not the next cell's actual use set.
2. `CheckedValueInputs::capture_checked` binds the selected interface bytes to
   their issued certificates and creates request-local files. The physical
   path/digest checks and their later revalidation remain necessary.
3. All three checked offer routes call `checked_value_context`:
   inspection, a checked cell, and a planned cell program. It currently calls
   `extend_retained_value_artifacts` on the entire retained list.
4. That function repeatedly merges each value's `CompilerSourceSupport`,
   retains its template custody, merges the template graph/support, rebuilds
   context owners, composes lexical nodes, then finalizes and normalizes.
   Pairwise support merging validates both projections and iterates the
   cumulative selected roles. Each rebuild resolves associations and retains
   native-offer custody again.
5. A value has already been narrowed by its original output issuer:
   `PreparedValueSourceSupport::retain_output` selects its canonical interface
   and required type/source surface. `capture_output` separately captures
   protected templates and retains historical original custody. The current
   `compiler_support` additionally receives value custody, making that larger
   object the input to the next offer's fold.
6. Actual imported values appear later in referenced STG facts, certified
   target/group imports, and exact retained identity/generation pairs. Runtime
   installation validates the actual binding, generation and handle. Source
   demand follows exact qualified group edges. Neither the request's complete
   injected inventory nor name shadowing is that use selection.

## Proposed domain boundary

Use one private compiler-surface value inside the existing captured artifact.
Keep the existing historical `ArtifactView` independently owned. The following
is an API sketch, not compiled code:

```rust
struct CheckedValueCompilerSurface {
    support: CompilerSourceSupport,
    source_lexical: Arc<[ExactLexicalNode]>,
    templates: Option<Arc<RetainedTemplateImports>>,
}

pub struct CheckedValueArtifact {
    // Existing interface, certificate, receipt identity and directory lease.
    compiler: CheckedValueCompilerSurface,
    artifact_view: ArtifactView, // Exact historical lifetime/execution custody.
    // Existing optional host prototype remains independently owned.
}
```

The fields and constructors stay private. Both production issuers must use the
boundary: ordinary/prepared checked output capture and the existing genuine
host-binding-interface receipt validator. No raw artifact/projection pairing
constructor, development feature or test authority factory is added.

`PreparedValueSourceSupport::retain_output` will issue the compiler contribution
from its already-selected source/type support before adding lifetime-only value
custody. The final artifact still holds the full original view. Templates retain
their exact protected graph and compiler requirements; they are not downgraded
to ordinary lexical imports. The directory and certificate retain their present
lifetime. Do not replace a certified value with a naked interface byte vector.

An input can be omitted from the compiler contribution only if it is absent
from the issued role closure, type dependencies, protected template/instance
requirements and exact compiler-native offers. A hidden carrier needed to
authenticate one of those offers is still a compiler requirement even when it
has no namespace role or executable root. The split must be expressed by the
existing selection owner, not by filtering module names or artifact kinds.

## One composition at the request boundary

Replace the sequential all-history fold with an operation on opaque issued
supports. Proposed private APIs:

```rust
impl CheckedValueInputs {
    fn compiler_surfaces(&self)
        -> impl Iterator<Item = &CheckedValueCompilerSurface>;
}

impl CompilerSourceSupport {
    fn compose_issued_sources<'a>(
        &self,
        incoming: impl IntoIterator<Item = &'a CompilerSourceSupport>,
    ) -> Result<Self, CompileError>;
}

impl ExactDeclarationContext {
    fn with_checked_value_surfaces(
        self,
        inputs: &CheckedValueInputs,
    ) -> Result<Self, CompileError>;
}
```

`compose_issued_sources` belongs in the existing inventory/projection owner.
It receives no caller-supplied role/view pairs. It gathers authenticated role
rows and their exact issuer bindings, carried input decisions, native offers,
scope proof references and acquisition owners; it installs one final namespace
and validates the joined closure once. It must not be implemented by calling
the old pairwise `merge` N times underneath a new name.

Reuse `CompilerInputProjection::admit_role` and the existing group-choice
validator. Preserve input order where it chooses an issuer. Equal role payloads
do not establish equal scoped bindings. An explicit empty input decision still
suppresses fallback; absence is different. A role promotion/replacement must
not inherit a decision for a previous role. Conflicting original payloads,
published selections or exact group bindings retain their typed refusals.

Compose lexical nodes and template nodes once from their issued contributors.
Templates use the existing producer, protected-source, canonical and graph
closure checks. Identical owner references can be reused; equal digests or
payloads cannot collapse independently issued acquisitions. Source/template
support enters the role composer once in its established order, instead of
being merged into a value and then merged again through its template owner.
Call `finish`/normalization once for the resulting context.

This still scans the interfaces needed to typecheck the whole lexical scope.
It does not promise constant-time offers or postpone GHC type dependencies.
It removes repeated cumulative graph/role reconstruction and lifetime-only
custody union; it adds no session cache, inverse index or publication-time
aggregate. In particular it does not move a potentially failing cross-capture
join into a successful binding commit, where refusal timing would change.

## Use and durable handoff

The applied offer continues to retain every input `CheckedValueArtifact`
through `CheckedValueInputs`, so detaching historical custody from the compiler
namespace does not release it. The complete injected-owner request selection,
request identity, receipts, late physical interface validation and cancellation
owner remain unchanged.

Use the existing final `TargetNativeInputs`/`ResolvedNativeTarget` and certified
import correspondence as the only starting point for selecting captured-value
execution custody. Retained imports are matched by full identity and generation
against the same admitted inputs; current machine handles are still resolved
and checked by runtime. Transitive source groups use the existing exact target
closure, not every certificate group. This immutable custody selection is not
permission to execute a value.

If the output's original execution context or a durable publication needs a
historical view, attach only the matching issuing value's view at that use
boundary, preserving compiler namespace and source/instance selection. Do not
reconstruct it from the union of available payloads. Where a surviving compiled
closure retains an older value, retain that older exact binding even if its
name has been shadowed or its public alias released.

This is a mandatory review checkpoint in the implementation: target originals,
`PreparedCheckedItemOutput`, authored declaration publication and recovery
materialization must retain every actually referenced historical graph. Current
recovery serializes `ExactContextSnapshot` selections, roles, offers and input
decisions, then authenticates those exact references on reopen. A narrower
request context must not accidentally become a narrower durable execution
record. Prefer passing existing typed selected views into that owner. If the
required distinction cannot fit the existing durable context references without
a format/consumer change, stop and obtain an explicit migration decision.

No live Rust `Arc`, pointer identity or generated temporary path becomes a new
durable identity. Existing context roots, scoped artifact/group bindings,
canonical/native certificates, retained generations and allocation fences remain
authoritative. Recovery must not promote Empty/Partial availability to Full or
turn historical byte custody into source/lexical/instance visibility.

## Compiler-time requirements

An interface-only plan for every retained owner is unsafe. The existing Haskell
hydrator deliberately distinguishes lexical instance/family reachability,
implementation-only HPT custody, protected scaffold import graphs and original
preview orphan scope. Keep all these issuers and restrictions unchanged.

Class dictionaries, type families, original generated Names and possible
compile-time evaluation may require a native original before runtime imports
are known. The compiler-surface owner must retain the exact issued availability
for these requirements. The worker's current demand and proof transport remain
unchanged; there is no new round trip to ask Rust for bodies during compilation.
Source/dependency observation and replay eligibility remain distinct, including
TH/untracked-input refusal. If obtaining the proposed surface requires changing
that compiler protocol or guessing which originals a source expression will
use, escalate rather than implement a speculative pruning rule.

## Independent history oracle

Use three complementary oracles; none can substitute for the other two.

1. **Small graph model.** Retain a list of logical capture records, each with
   an independent owner token, compiler roles, explicit group-choice domain,
   lexical edges and an immutable historical edge set. A straightforward fixed
   point computes expected closures. Keep producer-returned bindings mapped to
   separate logical identities only after uniqueness assertions. Generate
   repeated same-owner references, distinct owners with equal payloads and
   different children, overlapping scopes, explicit Empty/Partial/Full offers,
   both join orders, demand of either history, and ambiguous unqualified demand.
   Compare exact selected bindings/edges and retained historical closures, not
   only payload sets or module names. Use existing checked-value owner tests
   and generators; do not create raw production authority constructors.
2. **Behavior through the production compiler/runtime.** Extend the maintained
   `typed_segment_tests` capture history, whose GHC oracle retains simple values
   independently of compiler identities. Include a bounded sequence of unused
   accumulated values, a closure capturing an old value, shadow/redefinition,
   later use, a rejected cell, a second alias and release. Its mandatory
   capture/shadow oracle must differ from a mutation that selects the newest
   same-spelled value. Existing home-value source-drift/removal and nominal-type
   tests cover the original-source dependency. Add a class/family/scaffold
   control that genuinely needs the old compile-time graph, and preserve TH
   behavior/refusal through the real compiler if that feature is admitted by
   the existing fixture. Assertions must reach their intended typed boundary.
3. **Cold durable history.** Extend the existing fresh-worker/process exact
   recovery acceptance case. Publish an authored closure that captures an old
   value and a distinct later value sharing some immutable products; close the
   actual owner, reopen through the recovery owner, and execute both intended
   uses. Check exact generations and scoped dependencies before/after recovery,
   then corrupt a referenced interface/package/graph and require refusal.
   A failed cell or cancellation leaves visible names and prior histories
   unchanged; release retires only the last legitimate owner/alias.

The old pairwise composition can be retained temporarily as a cfg-only
executable comparison while developing the batch operation. It is not an
independent oracle and must not be the sole acceptance criterion. The final
code must have one production compiler path.

Work assertions use the existing cfg-only owner counter pattern. Count incoming
role rows, distinct exact support issuers, graph binding visits, namespace
installation/finalization and selected execution-custody contributions. For a
fixed compiler/type surface, adding N lifetime-only historical captures must
not cause visits to all their execution graphs before a target is known. A
batch must process the incoming role contributions plus the final unique role
set, rather than rescan all growing prefixes. Do not constrain legitimate
interface scans or confuse shared byte retention with physical byte reads.

Sensitivity mutations: omit an old issuer's child graph; select the newest
capture; union competing scoped choices; drop explicit-empty decisions; widen
private template imports; or skip late interface/package revalidation. Each
must fail its own control. Removing the batching optimization must fail a
structural work oracle without changing semantic test outcomes.

## Implementation sequence after review

1. Factor the private surface at both genuine checked-value issuers. Keep the
   old route until the source/instance/template/custody split is reviewed.
   Do not claim a performance improvement from a field move.
2. Implement the single inventory-owner batch operation and context composition;
   migrate all three checked offer routes together. Compile owning normal and
   cfg toolchain targets early, then directly affected runtime targets. Preserve
   first diagnostics. No new Haskell wire format or build feature is intended.
3. Complete the typed use/durable handoff above, then remove the production
   all-history fold. Run the small graph/semantic/refusal controls before a
   representative packaged history. Stop on an unexplained authority failure.
4. With Delivery's released-resource admission, run one matched packaged Persona
   history and the necessary cold-recovery leaf on the exact candidate. Retain
   whole execution, offer phases, work counters and selected/custody distinctions.
   Counts establish removed work; wall/CPU/RSS/retained bytes need actual runs.
   Record trace overhead and unaccounted remainder. No speed claim transfers
   from Persona2204 or from a failing setup.

Likely production paths are `checked_cell.rs`, `declaration_context.rs`,
`artifact_inventory.rs`, `artifact_inventory/compiler_projection.rs` and the
three offer callers in `artifacts.rs`. Runtime changes are limited to the
existing admitted use/recovery handoff if necessary. No additional session
aggregate is planned.

After migration, remove `CheckedValueInputs::certified_artifacts` if its only
production use disappears (it currently allocates a temporary vector for the
fold), the full-artifact `extend_retained_value_artifacts` production entry,
and duplicated per-value template custody/support merging. Keep general
`CompilerSourceSupport::merge`, `RetainedTemplateImports` validation and recovery
readers wherever independent production consumers still need them. Test-only
observations should name compiler surface versus historical custody explicitly;
do not delete an adverse history merely because the aggregate graph changes.

## Concrete step 2 boundary

Step 1 is source-reviewed. Step 2 is an isolated NEXT source implementation;
normal/cfg compilation, execution and integration qualification remain pending.
Step 3 narrowing, use handoff and recovery changes require a further review.
The release candidate is untouched.

`CompilerSourceComposition` borrows the ordered opaque input supports. It carries
persistent roles and explicit decision domains, plus the exact selected row and
input issuer for each winning role. Each contribution checks incoming roles;
only the first actual merge also resolves untouched baseline roles. An absent
native decision is filled by the existing issuer policy, while an explicitly
empty decision remains empty. A changed role cannot inherit its predecessor's
decision. There is no lookup by payload name or latest history.

`CompilerViewComposition` checks each contribution's issued scope proofs and
payload/acquisition compatibility before a later contribution is inspected.
This preserves an earlier payload refusal rather than replacing it with a later
role refusal. It retains immutable input parents and flat historical selected
seeds, not intermediate prefix views or read projections. Exact namespace and
root publication happens once after the fold. Final association, selected-owner,
materialization and native-offer custody checks remain in their existing owners.
This is not a claim that the entire function performs one graph traversal.

The retained-template composition joins equal-on-overlap graph nodes and their
existing compiler supports. It counts only newly introduced edges and refuses
monotone node/edge limits at each prefix, after that prefix's support check.
Union of closed input graphs is closed when shared nodes agree; the final
canonical/closure validator remains. Same-Arc template inputs keep their existing
fast path. The extra template lifetime join is removed only because both private
value issuers already place that exact template support in the value surface.

There is one context rebuild/finalization after source, template and lexical
composition. All three existing checked offer callers use that same entry point.
No historical view is narrowed; no publication-time aggregate, new wire data or
recovery authority is introduced. The previous checked-value fold and its template
merge are cfg-only differential controls. General pairwise support merging stays
available to its independent production consumers.

The seven new focused controls are:

- independent fixture-owner/ID expectations and cfg-only actual role/namespace
  work counts, with the old quadratic role scan as a sensitivity reference;
- explicit Empty versus absent input decisions and first-merge policy;
- independent equal-payload scopes with conflicting selected ordinals;
- earlier hidden-payload conflict versus later role conflict, in both orders;
- a generated five-mode ordered decision model with independently named scopes;
- actual published-source issuance/provenance and conflicting prefix refusal;
- existing independently captured hidden-child histories compared with the old
  fold, supplementing their unchanged independent custody assertions.

The work counters cover incoming roles, first baseline roles, final roles,
namespace joins and old pairwise role visits. They do not measure physical byte
reads, total graph visits, allocations or wall time. Representation fixtures do
not replace later production compiler/use/cold-recovery histories. The resource
limit prefix logic is source-reviewed separately from those native obligations.

## Remaining review and execution decisions

- Independently review the exact step 2 implementation and mixed-failure history,
  especially protected-template/native hidden carriers and foreign inventories.
- Compile owning toolchain normal/cfg and directly affected runtime consumers at
  the next admitted NEXT resource boundary; preserve the first diagnostic.
- Run authoritative discovery and the seven exact focused controls, plus existing
  capture, host-binding, template and durable histories selected by the reviewer.
- Review step 3's use-boundary custody retention before implementing it. The
  no-format-change recovery limit remains: a missing durable carrier is a
  blocker, not permission to discard older scopes.
- Measure a representative packaged history only after correctness acceptance.
  There is no speed, memory or integration-ready claim from this source parcel.
