# Exact successor source review and joined lexical failure

Reviewer: `/root/warm_compiler_campaign/owned_inputs/source_lifetime`.
Producer: `/root/warm_compiler_campaign/owned_inputs/rows`.
Base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`.
Prior cumulative source receipt: `49b65c8f8220c0beb3e9b6b4ecdf5fc83654c779`.
Candidate: `29d6467c309e53dbd0c1e9c89961c2eff24259c4`.
Implementation checkout: `/srv/swarm/checkouts/tidepool-shared-warm-owned-rows-20261019`.

## Successor disposition

Source-only acceptance of the bounded successor atop the cumulative49 receipt.
The production delta removes the kind guard only when every direct leaf row is
already the same Arc in the complete owner's direct map. Direct rows override
inherited rows, so this establishes that no selected physical fact changes.
Inherited-only rows still take the existing initial detachment. Structural
equality, IDs or seals cannot substitute for Arc identity.

The new `direct_complete_promotion_reuses_owner_without_fact_detach` control
first prepares the original direct generic value row, then enters actual P6
capture. It asserts zero fresh promotion, identical interface path and the same
complete Arc through four retained/prepare rounds. Its thread-local counter
increments at the actual initial `select_custody_facts` branch and remains
unchanged. The test would fail under the preceding kind guard. Existing changed
row history controls remain necessary negative coverage; an equal-byte new row
must still replace the current physical frontier while preserving old in-flight
requests. The empty-leaf branch is unchanged.

The trace reports actual direct lookup visits (including short-circuiting) and
zero detach on the shortcut. Scoped cache scanning and leaf matching remain;
this is neither constant-time acceptance nor a measured speedup. `git diff
--check` passed; the reviewer ran no build or tests. Successor compilation and
counted execution remain producer-owned and pending at this receipt.

## Exact49 joined failure

Inspected `native-results-49b65.json` and `test-runtime-fresh-worker.log` in
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/owned-inputs/rows/`.
They retain exact source, pre-execution artifacts, counts and outcomes:16
toolchain cases and1 runtime checked-expression history passed,1 runtime fresh
worker case executed and failed. The failure is `session/turn.rs:8693`, before
hidden-import inspections and checked-bind/input-work assertions. Cache retained
at `/tmp/tidepool-libtest-cache-jv8erlca`. This is not joined acceptance.

The failing assertion requires `context.lexical_graph()` to contain
`view.library().module_name()`. The owning contract distinguishes:

- `SessionCompileView::library()` returns the reserved logical/native declaration
  generation through `CompileLibrary::original`.
- `library_import_module()` returns the actual compiler interface. A Certified
  library uses `projection.module_name()`; a recovered library uses its evidence
  root. `turn_imports` consumes that import identity.
- `lexical_projection::issue_projection` issues a
  `Tidepool.Actor.Surface.H<digest>` module, then creates the context lexical root
  from that exact receipt, plus admitted surface lexical dependencies.
- `paired_publication::reusable_projection` checks current admission, final
  generation, exact exports, instances, families, surface and original products.
  `PublicationEvidence::ReusedProjection` deliberately couples the new logical
  publication generation to the unchanged compiler receipt. `stage_materialized`
  clones that projection's context. `matches_generation` checks the logical
  reservation separately rather than requiring a renamed receipt.

Thus the existing assertion is incompatible with the accepted reused-projection
route. Its current form was introduced in `033980ec182`; the library identity
split is present since `8a8e3ee9874`/`cfa54554d97`, and the reusable-projection
gate since `058cbcea977`, all ancestors of the assigned base. The exact same
assertion and split are present at base5b74. Host_inputs independently confirmed
this owner chain. No production lexical repair is justified by this failure.

## Row-route discrimination

Before the assertion this test opens an empty session, validates and adopts a
declaration, retracts a type head, publishes declarations, retires the private
scope and removes original source/interface aliases. It creates no runtime
value binding or checked bind. Declaration term-type queries in
`validate_rendered_module` are strict inspection requests, not Val interface
issuance. The declaration fixture's functions and constructor belong to its Lib
original/canonical products. The new P6 `capture_output` and checked-value leaf
promotion routes are not entered by this prefix.

Declaration projection preparation does materialize interfaces, through
`declaration_join_inputs` -> `materialize_interface_inputs`. This is the existing
interface-only owner, separate from `materialize_retained_artifacts` and its
dedicated ValueInterface factory. Exact compilation for earlier probes may
prepare generic native/canonical/lexical facts, but there are no issued Val
entries or leaves in this setup for the changed value branch or overlay to use.
The row cache stores physical observations; `lexical_graph` reads immutable
`owner.facts.lexical`, issued by the paths above. The cumulative row diff neither
rewrites that lexical field nor changes runtime projection selection. This
source chain establishes a stale test expectation; it does not claim an executed
base comparison or prove the later checked-bind workflow passes.

## Minimal next observation

If execution attribution remains required, the smallest controlled comparison is
the unchanged exact test at base5b74 in an isolated admitted checkout with the
same pinned compiler/profile/resources and a fresh per-run cache. Retain the
executable hash and executed1 outcome. Do not rewind the producer's active
checkout or reuse its failure cache as a cold control. Equal failure at8693
confirms the preexisting assertion dynamically; different behavior requires
capturing the actual library identity, actual import identity, receipt root and
lexical owners on both versions before diagnosing a row regression.

The owning runtime test repair should assert the issued actual projection root
via `library_import_module`, retain the logical generation check separately,
and keep the hidden-original negative lexical/import controls and exact native
group retention checks. That discriminates genuine projected visibility from a
weakened assertion. The reviewer made no runtime edit and started no build.
The owner must then rerun the entire nonzero fresh-worker case through the
checked-bind/input-work assertions; fixing this early oracle is not a workflow
pass. Rows owns successor checks; host/area own lexical test repair and joined
qualification.

## Successor native evidence acknowledgment

Subsequently inspected the producer's terminal evidence for exact29d6467c,
without starting new native jobs. `native-check-state.json` records the pinned
fast-dev exact toolchain/runtime check-plan build with `-j1`, local execution
and remote disabled, exit0. Its exact label list is
`check-targets-29d6467c.txt`; `check-direct-reuse.log` ends BUILD SUCCEEDED,
build ID `12de0974-5dbd-41a8-a6c9-8ffeea0c6c39`,27 local actions.

`native-results-29d6467c.json` and all six referenced individual case reports
agree on executed1, passed1, failed0 and exit0 for baseline borrowing/cleanup,
concurrent produced capture, direct-complete same-Arc/zero-detach reuse,
original dedicated row/sibling release, same-view cold materialization and the
sixteen-promotion current frontier. All record the pre-execution toolchain test
executable SHA256
`776235e328c0de1ebbea9b459c7091e28a64e314146d96309e9419f450710ca0`.
The checkout remained clean at exact29. Source acceptance therefore has affected
production/cfg(test) compilation and these six nonzero executed controls behind
it; this does not expand into joined runtime/actor acceptance.

Individual reports explicitly retain incomplete resource-hash coverage for
directory-valued resources and unobserved hosted/compiler cleanup fields.
Those fields are not converted to complete closure or END settlement evidence.
Prior exact49 results remain separate17pass/1lexical-fail evidence. Runtime
oracle repair and the complete fresh-worker checked-bind rerun remain with
their owning area; no later workflow pass is inferred here. Producer reports
all jobs terminal and holds further work for actual integration findings.

## Adjacent runtime oracle repair assignment

Area subsequently reports qualification's exact6d3 baseline executed the unchanged
case and reproduced the same8693 pre-P6 assertion failure. Independently read
the retained report and stderr: executed1, failed1, passed0, exit101, same
assertion. Report SHA256 is
`4fff5a9e049235a69dff22cb6bad587f853ad7d9b79e472d66ebac940039b124`,
path `/srv/swarm/evidence/inanna/shared-warm-qualification-20261019/baseline-6d3/paired-join-case/8c6c6b90010c582d8e529fa11db53f1afac0ec1d8a11018970d3e83e32165b0b.json`.
Source attribution remains qualification's retained baseline contract; the
individual case JSON itself does not contain a source OID. This corroborates the
existing-oracle diagnosis without turning an expected failure into passing
checked-bind evidence.

Approved adjacent assignment is test-only: compare lexical graph with the actual
`library_import_module`, explicitly distinguish that root from the reserved
library generation on this certified reuse route, retain the original-generation
inequality, hidden-original absence, exact native selections, all inspection and
refusal controls, actual GHC checked bind, capture and input-work assertions.
No production lexical authority or test authority factory changes are needed.
At inspection the producer checkout was still clean exact29; no repair candidate
was available to review. Producer rows will deliver an exact successor, then
this reviewer checks its complete29-to-successor delta and unchanged later
controls. Fullworker expected1 execution remains rows-owned. No reviewer native
job or runtime source edit was made.

## Exact adjacent test-only successor review

Candidate `a178a9d331ac560786925e3c443a5789e03c9358` arrived after that inspection.
Reviewed the exact29-to-a178 delta and cumulative whitespace check. Verdict:
**source-only acceptance** of the approved adjacent oracle correction atop the
accepted cumulative row implementation. The checkout is clean at that OID.

Only `turn.rs` inside the existing paired test and its handoff changed. The test
obtains `library_import_module().expect(...)`, requires the logical/native
library module to differ in this certified-reuse fixture, and requires actual
import membership in the lexical graph. This asserts the distinction rather
than merely removing a failure. Existing logical-library versus original
generation inequality, Text dependency, hidden-original lexical absence and
textual-import refusal, exact native-group comparison and canonical interface
role checks remain unchanged. The entire subsequent inspection, GHC checked
bind/expression capture, receipt/admission mutation refusals, semantic digest,
native identity and input-work assertions remain unchanged. No public API,
production branch or test authority factory changed.

The handoff preserves baseline failure provenance separately and explicitly
requires production/cfg(test) runtime compilation plus the full fresh-worker
selector with expected1. Those new exact-candidate outcomes are pending and
rows-owned at this source receipt. Reaching a later assertion would be a new
observation, not permission to weaken that oracle. Reviewer ran no native jobs.
Next action is rows' pinned compile/full counted execution and retained artifact,
count, failure/cleanup report; the joined owner consumes the resulting evidence.

## a178 terminal execution and pre-demand native oracle

Inspected `native-results-a178a9d3.json`, its individual report and complete
failure tail, and `check-runtime-oracle.log`. Exacta178 production/cfg(test)
runtime check passed,13 local actions, build
`adbf469a-0f09-47cd-b456-b927057e9dae`. The full corrected selector executed1,
passed0, failed1, libtest101/runner1, at the preserved native equality assertion
now `turn.rs:8717`. The actual lexical root checks passed; checked-target
compilation/capture still did not run. Report SHA256
`1b2bd8ea2d76b3885231c57f56176a569dc2a2c3aba2155259b0d07820898e6a`;
pre-execution runtime executable SHA256
`e4b22a9bfc8097ff3ee3d14a879ff3049915975fff023bde8bb59c6d6697d045`.
Failure cache `/tmp/tidepool-libtest-cache-_02eaxl5` remains retained. Process
cleanup is confirmed; compiler cleanup is explicitly not observed. These are
separate outcomes. Rows reports all external jobs terminal, source clean.

The failing comparison is empty **selected execution demand** versus21 group
keys selected by the original issuer. That observation alone cannot establish
lost native custody. Exact source relationships are:

- `ArtifactView::selected_native_groups` reads `selected_nodes`, the closure of
  executable/source roots. `capture_graph_selection().native_groups` reads
  `read_projection.nodes`, the retained closure including custody roots.
  `binding_dependencies()` exposes exact binding-to-binding edges over that
  retained closure. These are separate existing public observations.
- `NativeArtifactDemand::ScopeInterfaces` explicitly retains original bytes and
  interface authority without an executable target capable of native demand.
  `CertifiedTargetImports` and `VerifiedTarget` supply later demand from exact
  certified imports/entry evidence, rather than from the available carrier set.
- `CertifiedAuthoredDeclaration::publication_compiler_support` and the analogous
  AcceptedJoin method call `CompilerSourceSupport::for_source_owners`. Hidden
  authored owners become InterfaceOnly through projection policy, while their
  exact artifact graph and issued offers remain retained. The compiler support
  implementation preserves artifact/offer Arcs separately from role changes.
- `ExactDeclarationContext::extend` merges those supports under the lexical
  selection. `merge_admitted_compiler_supports` consumes exact role/input
  decisions. In `merge_compiler_inputs_inner`, an InterfaceOnly role has no
  `role.original()` and contributes no demanded groups. The merge moves
  predecessor roots/custody absent from the new execution roots into custody;
  it does not reinterpret that preserved graph as target demand.
- Projection issuance creates its interface-only lexical context and publication
  can reuse that unchanged context. `authored_native_root` locates an exact
  retained authored original by its certified origin; its own documentation
  explicitly says a lexical projection grants no native root.

Accordingly the source supports a **stale pre-demand oracle**, not a confirmed
product-loss finding from the empty selected set. The current report does not
observe the retained graph or offer contents dynamically at that point, so it
does not assert that21 exact custody rows were actually present. The correct
next observation must discriminate those cases before accepting a repair.
The native assertion predates the assigned base (`0180c89bfeb`); the role/custody
mechanisms above also predate it. The rows parcel changes physical observations,
not native root or role construction, and this prefix issues no Val/P6 row.
This is source evidence, not an executed same-oracle baseline comparison.

### Bounded discriminator and repair direction

Use the existing public observations in the same production test. Capture the
original issuer's exact `NativeGroupBinding` set for `original_artifact` from
`capture_graph_selection().native_groups`, require it nonempty, and compare it
with the corresponding retained set in the public context. Compare exact
`binding_dependencies` for those group nodes and their original carrier nodes
to preserve the group-to-carrier, canonical interface and native dependency
edges. Binding identity includes its issuing scope; an ArtifactId/ordinal-only
set or group count is insufficient. Keep pre-target selected demand checked
separately (it may correctly be empty). Missing retained bindings or edges is
then a product custody finding, not a reason to delete that check.

Existing later checks remain essential: native original group identity in the
compiled certification (currently around8941), checked target matching,
edited-source/binder/signature refusal, unchanged context digest and visibility,
canonical expression capture, hidden-original authored-import refusal, actual
resident bind/expression completion and replay refusal. They are intact in
exacta178 and still unexecuted after this earlier failure. Exact binding/edge
custody checks must supplement those later executable-demand checks; merely
removing the native comparison is not an accepted repair.

Qualification may discriminate preexisting behavior by applying only the already
approved lexical-oracle correction to its isolated exact6d3 baseline, retaining
base OID and patch digest, then running the unchanged native comparison once.
That is a modified baseline control and must be labelled as such. Reproducing
the empty-selected failure confirms the old demand expectation dynamically;
the improved custody/edge oracle and later full workflow still need their own
execution. No reviewer runtime edit or native job was made. Area owns any
approved test-only successor; rows remains its implementer, qualification owns
any baseline action, and exact source review precedes full execution acceptance.
