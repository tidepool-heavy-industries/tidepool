# Independent checked-row repair review

Task: `/root/warm_compiler_campaign/owned_inputs/source_lifetime`, rows Wave 1.
Base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`.
Candidate: `6d42df57ad13ea3dc7f90f5c70e6d12963706d3c`.
Compared cumulative source and repair delta from `26e0f31df52a04a88dd07913e990b2e9048e9103`.
Checkout: `/srv/swarm/checkouts/tidepool-shared-warm-owned-rows-20261019`.

Verdict: repair still required for the new cached-overlay physical ownership.
Both earlier constructor/omitted-route findings are functionally addressed.
Compilation and execution remain unqualified under the producer's retained disk
admission blocker; no build/test command or resource change was attempted.

## Closed findings and source controls

`CheckedValueRowIssuer` now privately owns its certificate and cancellable row
slot. `CheckedValueRowSource::Produced` derives the certificate from that owner,
so an independent value and arbitrary populated mutex cannot be paired. The
certificate owns the admitted bytes; no public test authority factory was added.
The fresh route has no reused packet. Production callers preserve exact selected
support checks before hitting a leaf.

The concurrency control now enters actual ordinary/prepared `capture_output`
with one produced packet and asserts both shared row authority and physical
promotion count 1. Wrong selected context and pre-cancelled capture leave the
counter 0 before valid retry produces 1. The shared lock's deterministic test
cancels an observed waiter while its holder remains locked. These improve the
source oracle; none has executed at this candidate. Partial-write failure cleanup
is still supported by source-local TempDir/memfd ownership rather than an
executed injected failure history.

Same-view cold preparation now includes own leaves after inherited sources.
Existing complete hits overlay own leaves while leaving old in-flight requests
immutable. Sharing `execution_scope` behind `Arc<Value>` avoids copying its
encoded graph metadata; inspected owning production/test reads use dereference
compatible access. Native compilation is still needed to establish that change.

## Finding: cached overlays retain obsolete rows and predecessor chains

`declaration_context.rs:4048`, `overlay_checked_value_leaves`, constructs a map
of all current leaves and returns an owner with `_parents = [complete.clone()]`.
This retains the whole predecessor solely to preserve any old graph/file owner.

Concrete unnecessary-backing history through existing constructors:

1. Build context C with V and unrelated large S. Prepare C once; its old V/S
   rows share arena/directory E.
2. Derive `C.compiler_source_support().select_value_interface(V_certificate)`,
   then extend that issued support into fresh context N using
   `extend_issued_compiler_support`. N selects only V, has no native/group/graph
   dependency or prototype, but its inherited old V row still pins E.
3. Prepare N once, obtaining its cached complete owner. Drop C, its requests and
   temporary support owners. Promote V's exact output packet on N and prepare N
   again; the emitted V now uses the value-sized leaf.
4. Drop the pre-overlay N request. The new cached wrapper still retains
   oldComplete → obsolete oldV row → E, so unrelated S backing remains. Nothing
   selected by N requires that old V row. This is an overlay lifetime bug, not a
   required native-offer/prototype exception.

The current broad test retains both V and S in its source context and drops that
context before asserting old backing release. It therefore does not distinguish
the V-only cached-complete history above.

There is also structural accumulation on one stable prepared multi-output
context. Repeated promote V1 → prepare, promote V2 → prepare, … creates maps of
1, 2, …, N leaf handles and an N-deep chain of complete wrappers. Dropping old
request objects does not release those maps because the current cached owner
retains every predecessor. This introduces quadratic retained row handles and
destructor depth. Already issued requests can keep their old independent owners;
the current owner need not retain those entire predecessors.

Repair at the existing physical owner: retain only still-selected row, group,
graph/file and shared scope owners needed by the new complete view. Keep actual
required A/graph owners and exact binding/acquisition precedence. Avoid a chain
of prior full maps or an all-history publication refresh. The implementation
choice remains with rows and its owning supervisor. Add the V-only backing
history plus repeated overlays with measured owner depth/row-handle growth and
old in-flight request stability.

## Remaining qualification and routing

Same-view hot overlay presently scans cached keys and current leaves even when
unchanged; report that actual work rather than claiming constant-time reuse.
Native-offer/catalog/prototype retainers remain intentional and separately
measured. Actual checked worker use, per-view acquisition absence, BindingIndex
authority refusal and END-uncertainty behavior remain unexecuted joined checks.

Both rows and the area supervisor received the concrete finding and history.
Next producer action: repair overlay ownership and its discriminating controls,
deliver an exact new candidate, then request cumulative independent re-review.
Native compilation/tests remain required when an admitted storage lane exists.

## Concrete repair proposal review

Reviewed uncommitted repair over `6d42df57`: explicit `MaterializationKind`,
persistent `imbl::OrdMap` rows, shared `Arc<BTreeMap>` graph owners, and first
overlay converting `select_custody_facts([complete, leaves], metadata.retained)`
through `into_owned`. This direction addresses the counterexamples: current
physical row/group/graph handles are retained independently, the old complete
owner is absent, scope bytes remain shared, and subsequent row changes share
persistent structure with no predecessor materialization chain. Exact candidate
and executed controls remain pending; this is a bounded proposal judgment.

`metadata.retained` may include hidden custody with no physical materialization.
`select_custody_facts` gathers existing matching rows/groups/graphs; it does not
require every retained artifact to have a row. Therefore hidden unmaterialized
facts do not by themselves prevent detaching the complete physical frontier.
With a nonempty selected leaf set, `None` should be impossible under the existing
entries-contained-in-retained invariant because the leaf itself is a matching
row. Prefer making a violation explicit instead of silently returning the old
complete owner and concealing a missed overlay.

The existing execution scope was generated from consumed Original entries.
Those entries remain within retained metadata, so their execution-graph digests
select the matching `OwnedExecutionGraphFile` handles during detachment. Native
groups retain their exact pending certified owners. Row/graph handles still
provide original FD/directory leases to the existing endpoint; no new END owner
or semantic selection is needed. First overlay detachment is a cold closure
observation; report that cost separately from persistent later replacements.
Sent this verdict directly to rows and the area supervisor. Next event: exact
repaired OID with V-only old-backing release, repeated-overlay growth and
in-flight-old-owner controls, then cumulative independent re-review.

## Independent fixed-frontier retainer and issuing-boundary recommendation

Rows' concrete V-only test construction exposed an additional strong owner:
N's immutable `ViewLease.materialization.fixed` retains its old V row directly.
`materialization_sources` also stores cloned source caches/fixed owners.
Suppressing their traversal after a complete cache entry is populated does not
drop those fields. Therefore the no-parent overlay proposal above fixes its own
predecessor chain but cannot, by itself, prove release of E while N remains.
This is a confirmed source retainer through the existing view construction,
projection, merge and prepare paths, not a garbage-collector inference.

Bounded recommendation sent to rows, area and campaign: enforce the dedicated
value-row invariant at the original `materialize_retained_artifacts` entrypoint
for **every** `ArtifactPayload::Interface(_, ValueInterface)`. Reuse one private
physical row factory from original materialization and P6 capture. It accepts an
already admitted `Arc<ArtifactEntry>` and the operation's existing
`PackageInterfaceValidation`, derives exact owner/parts/seals/requirements, and
issues a directory/arena containing that value alone. Exclude value entries from
both generic shared-arena appends and generic shared-directory writes. Generic
native/canonical rows retain their separately justified packing and custody.

Then immutable fixed frontiers can legitimately retain old exact V rows because
those rows cannot pin unrelated sibling value S. This removes the impossible
broad-V state at its issuer rather than rebasing semantic/acquisition owners or
introducing a mutable whole-frontier service. Older live binary objects are not
in-process upgraded; durable recovery reacquires rows through the new issuer.
Existing older binaries remain outside this candidate's lifetime claim.

Checked capture may reuse an already materialized dedicated row only after
validating its exact selected binding and issuing acquisition facts, not artifact
ID, byte length or a leaf marker alone. Retaining the existing issuing
`ArtifactEntry`/fact handle with the row is one bounded way to authenticate that
association; `ArtifactFactsHandle` distinguishes independent equal-byte joined
issuers. Physical row sharing does not reissue logical support. The implementation
owner decides the smallest API, preserving the typed output packet and avoiding
another registry.

Required controls: original materializer issues independently sized value rows;
complete-before-P6 borrows that exact row with zero new factory writes; packet
concurrency promotes once; same-view and next-view paths keep original A rows;
V-only survives while S's actual directory/FD owner releases. Factory fresh-work
counters must remain distinct from packet attach/borrow counters. FD growth and
the existing arena-descriptor format bound remain explicit, without silently
raising limits. This recommendation supersedes treating overlay detachment
alone as sufficient physical-release acceptance. No implementation accepted or
native check executed in this review.
