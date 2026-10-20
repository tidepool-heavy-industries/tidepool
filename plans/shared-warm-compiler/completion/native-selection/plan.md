# Exact native selection and publication

This leaf implements the native contract in [../contracts.md](../contracts.md)
for the sole World cell producer described in [../plan.md](../plan.md). It is
an implementation plan, not passing execution evidence. The reviewed source is
`6f7ae09b3bdbcfd8952d9a513db91564ea1b5858`, with code tip `92fa518105`.
Record the receiving plan/code OIDs and inspect intervening code before starting.

The native-selection owner owns selection policy in `tidepool/toolchain` and
supplies its shared signatures to the service-cells owner. The engine integrator
joins those changes. Do not upgrade the old finite actor-cell producer as a
parallel consumer. Delete that producer with runtime cutover; legitimate
independent compilation components continue through the existing physical-output
issuer and native runtime authority. Read the toolchain contributor rules and
`docs/swarm-builds.md` before implementation or verification.

## Findings and limits of the evidence

| Finding | Current evidence and consequence |
|---|---|
| Exact graph replay is not narrowing | `ArtifactView::select_issued` checks requested IDs, then restores recorded `graph.roots`, namespace and custody. Preserve that meaning. A caller needing current projection must not use it as a subset selector. |
| First-publication and reopen policy are entangled | `published_artifact_roots` falls back to capturing ambient support; `select_publication_graph` returns saved birth support early for a published role. The later published-role match arm is unreachable. Existing offer rebuilding can reintroduce ambient custody. Narrow first issuance before capture; validate birth separately from current projection. |
| Candidate custody cannot issue a fresh Full row | `OriginalCompilerInputs::from_candidates` authenticates contextual products with `ScopeInterfaces`, retains custody, then carries existing offers/rows/decisions. `fresh_candidate_dependency_uses_issued_availability_without_selecting_execution` expects a new Full offer which this route cannot create. Do not restore the deleted general candidate issuer. |
| Unmerged b1 has a real selection flaw | In `f8cc8d4015`, `canonical_matches = !authorized || ...` allows a supposedly unauthorized support to bypass canonical checking and enter the sole-compatible-candidate result. A sole foreign control is required. However, a foreign ancestor can legitimately retain a historical child; blanket rejection of foreign proofs is also wrong. |
| Unmerged b3 changes custody policy | `a8b37dd1f3` drops competing original carriers during interface projection. Preserve alternatives as custody and resolve only the consuming selection. Its root-based metadata chooser must not turn an inventory census into compiler selection authority. |
| A7 has two distinct observations | Joined `completed_request_carries_exact_entries_without_reconstruction` first stops at an obsolete expected error string. Retained branch probes progressed further and got `selected original is outside retained custody`. Reproduce that later identity failure at the receiving revision before assigning a repair. |
| A1c is a later-declaration failure | Log `final-join/logs/15-a1c-tip-fc73e0e652.log:4047` names retained directory `.tmpIF9Dst`. The failed source is the structured probe script; G4 exports `queryTag`, `describe`, `probe`. The initial published `info` example completed its first display/string assertion. The three structured probe results remain unverified. |

Evidence root:
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/`.
Use `final-join/TOOLCHAIN-SWEEP.md`, parcel reviews, and retained counterexamples
as leads. The joined full run has 32 failures; its assertion-only classifications
are not established root causes. Dirty scratch-tree traces are not join evidence.

For A1c, the retained request is under
`target/tidepool-test-runs/compiler-failures/.tmpIF9Dst`. Its
`exact-request/facts.json` describes an Effects.Core interface and product
reference; `exact-request/input-artifacts.json` contains Effects.Core as a
canonical interface; candidate diagnostic owners and source-selected support
are empty. These diagnostic files grant no authority. In particular,
`retain_input_diagnostics` writes `compiler_inputs().metadata.entries`, not full
private custody or lowered native transport. Canonical-only metadata does not
prove that private body support was absent. `MissingDependency` can mean a
missing exact `NativeOwnerKey` in the selected resolver despite other same-module
payloads. Neither fixture omission nor a causal connection to A8 is established.

## Owning entry points and permitted reuse

| Responsibility | Existing owner to extend |
|---|---|
| Physical source/receipt/product certification and target admission | `artifacts.rs::seal_turn_outputs_with_validation`, `admit_planned_declaration`; `certified_products.rs::certify_products_with_validation` and existing checked output owners |
| Exact completed identity | `ReceiptOriginalIdentity`, `ReceiptNativeSelection::original_entry`, `CertifiedSourceSelection::bind_produced_originals`, `IssuedGroupCandidates` |
| Native demand and immutable carrier edges | `artifact_inventory/native_availability.rs::bind_input_demand`, `bind_fresh_product_dependencies`; `artifact_inventory.rs` target admission and exact group resolver |
| Carried choices and source support | `artifact_inventory/compiler_projection.rs::IssuedInputNativeDecisions`, `CompilerSourceSupport` |
| Full/partial/private transport normalization | `declaration_context.rs::NativeInputSelection::normalize_issued_contributions`, `from_normalized_rows`, `OriginalCompilerInputs::issue_completed_native_availability` |
| Publication | `declaration_context/published_source.rs`; `CompilerSourceSupport::{publication_birth_support,select_publication_graph,publish_original}` and existing `PublicationNativeAuthority` |
| Candidate discovery and accepted input evidence | `module_candidates.rs::{protected_native_closure,select_with_catalog}` and its existing dependency/acquisition owners |

Keep existing authenticated scope proofs, offers, materializations, publication
birth owner and verified binding handles. Extend their constructors and borrowed
results; do not add another persisted certificate, graph registry, owner index,
candidate authority factory or second certifier. Do not persist complete graph
copies at each consumer. Runtime execution authority remains Rust-owned.

Delete candidate-gather/canonical-filter/sole-candidate routes when the shared
resolver replaces them. Delete the unreachable publication match arm and obsolete
first-issuance reconstruction paths. Keep raw malformed-data fixtures for decoder
refusal tests; remove positive fixtures which manufacture trusted selections or
implement their own merge/recovery semantics. Coordinate those removals with the
tests leaf rather than preserving adapters for obsolete fixtures.

## Locked resolution contract

Use explicit demand provenance at the existing native resolution boundary. The
exact Rust spelling is local implementation detail; these are three different
constructor obligations, not labels inferred from strings:

1. **Root:** consumes the root's carried exact chosen binding. Missing, explicit
   empty, unavailable and conflicting choices remain distinguishable. No absent
   choice permits selecting a sole retained candidate. A new product's initial
   choice comes from its actual completed-output issuer.
2. **Historical dependency:** consumes an authenticated parent group and its
   exact labeled outgoing dependency edge. Follow the recorded target group and
   carrier, including selection IDs. A proof rooted elsewhere may supply this
   child only through that issued relationship. Current canonical compatibility
   does not select a replacement child.
3. **Current product:** consumes the matching authenticated physical completion,
   exact entry identity and continuation input evidence. This is the only case
   which can issue a new product binding. A successful runtime evaluation or a
   merely retained candidate is not a completion certificate.

Represent these distinctions by extending `CertifiedNativeGroupSupport` /
`NativeGroupCandidates` and the existing verified-binding acquisition result.
Borrow the actual offer/requirement/completion owners. Do not introduce a new
semantics-owning graph wrapper. Return the selected exact binding coupled to the
evidence required to interpret it; avoid parallel raw and enriched result APIs.
Canonical equality checks compatibility after authority is known. Custody and
alternative candidates remain independent of executable demand.

Both new World output certification and legitimate existing physical compilation
components consume this resolver. The service client assembles already-issued
output facts with runtime item settlement; it must not derive native choices from
evaluation, names, generation numbers or broad inventory. Share this signature
before either owner changes its caller.

## Publication birth and current projection

Define required first-publication closure as the least fixed point of the
completed compiler-issued lexical/export/import, canonical type, family/orphan,
instance and exact native obligations. Include empty imports and required
dictionary support. This is required closure, not optimizer-minimal code and not
all retained interfaces, all available bodies or the ambient instance graph.
Use issuing owners' edges; do not rediscover obligations from module names.

Construct that support narrowly, validate it, then capture birth exactly once in
the existing `PublicationNativeAuthority`. Keep `publish_original`'s strict
acquisition identity and selection checks on that actual birth. Do not weaken
pointer/acquisition checks to make a projected graph look like its issuer.

Reopening first validates immutable birth through `publication_birth_support`.
It then checks and produces the current role/demand projection separately. The
existing early return from `select_publication_graph` must cease returning full
saved birth support as the current projection. Birth remains owned by the
published role handle; the live result contains only current namespace roles and
demand, with required exact historical custody. Do not use `select_issued` for
that projection because it restores every birth root. Remove the unreachable
published-role match arm after restructuring the flow.

Unrelated role withdrawal succeeds. Missing required roles refuse the consumer.
Withdrawal never edits birth, reissues it or changes its hash. Same source
revision/input identity with the same birth remains idempotent even when current
projection changes. A conflicting reissue refuses. Current overlays can retain
additional independent custody but cannot replace a birth dependency using equal
canonical bytes. Recovery authenticates the same birth and exact carrier graph
before projecting current roles.

## Completed-output authority and candidate custody

Carry actual produced `ArtifactId`s from their issuing entry through
`IssuedGroupCandidates`, receipt binding, output custody and availability
completion. `CachedHomeOwner`, canonical equality or a reconstructed entry cannot
substitute for that identity. Preserve exact entry/materialization ownership when
the existing immutable owner is available.

An availability-only candidate stays custody-only until real completion
explicitly authenticates that accepted cached product and its exact dependencies.
Unrelated G4 completion cannot promote every contextual candidate. A product
acknowledged by the real physical receipt can enter the same existing completed
output constructor as other accepted products. A candidate excluded from that
receipt cannot acquire authority from output success elsewhere.

If the current receipt or candidate transport cannot express that acceptance,
repair the producing handoff in the service-cells/physical-output owner before
enabling reuse. Do not synthesize a receipt or restore the deleted candidate
issuer in Rust. Until then the same compiler service may compile the required
product through its normal authenticated physical path; it must not fall back to
the removed finite actor-cell engine or silently broaden private inputs.
Such compilation creates a genuinely new product with its own identity; it cannot
replace an exact historical dependency or recreate a missing issued binding.
Required historical evidence must be acquired from its real owner or refused.
Always recompiling instead of retaining compatible dependencies does not satisfy
the mandatory arm-B reuse gate.

Normalize Full versus issued-groups/private contributions once at
`normalize_issued_contributions`. Full qualification, historical dependency
consumption and current compiler roles have different obligations. Do not put
request selection policy into `NativeOriginalRequirement`. Several retained
alternatives are legal; conflicting choices in one consuming closure refuse.

## Implementation waves

### N0: exact counterexamples and shared signatures

Record the receiving source and candidate parcel bases. Inspect the full
base-to-tip diffs of root E `bfb8471722` and custody `a8b37dd1f3` (including its
`f8cc8d4015` parent). No wholesale merge is approved.

Repair stale error-oracle expectations with additive typed custody/identity
causes where programmatic distinction is needed. Then reproduce A7's later
failure through the production completion constructor. Keep missing custody,
misbound original, foreign scope and pending product distinct. Do not weaken to
`is_err()` or string substring checks. The tests leaf owns broad stale-test
disposition; agree shared edits before changing its cases.

For A1c, retain the old directory and log association. Trace the production chain:
`admit_planned_declaration` -> `seal_turn_outputs_with_validation` -> product
certification -> `certified_selected_product_artifact_view_with_validation` ->
`certified_product_artifact_view_inner` -> inventory target admission. The
failure precedes successful `selected_authored_original_closure` and final
`certify_same_offer_planned_declaration`; root E is not presumed its direct fix.

Run the exact hosted selector on the receiving source with existing bounded
`source_closure_trace` / `ambiguity_trace` enabled. At the failure compare wanted
exact native owner, resolver candidates, retained originals, effective compiler
role, availability contributions and carried group choice. Inspect candidate
protected-cohort membership before and after manifest/choice filtering. Add only
missing fields to those existing structured trace points; keep captures private
and bounded. Verify traces were enabled before interpreting missing events.

Discriminate these explanations: no authenticated body acquired; body acquired
but absent from normalized availability; exact choice lost or rebound; wanted
owner differs from retained owner; accepted product omitted by receipt; or test
fixture missing a declared production input. A fixture repair is appropriate
only after demonstrating that production startup supplies the missing input and
the fixture bypassed that owner. Retain the smallest producer-to-consumer history.

Publish the resolution constructor/return contract to service-cells. N0 completes
when the counterexamples and refusal causes are observable and both owners agree
the actual World certification caller; a helper-only API is insufficient.
Assign named shared functions and receive signature commits serially in
`declaration_context.rs`/`certified_products`. The service output supplies its
authenticated lexical/type/instance/scaffold facts through the existing demanded
source issuer; native selection validates exact bindings and captures the required
birth closure there. Do not independently rebuild those facts in both leaves.

### N1: shared native resolver and contribution normalization

Implement the three provenance constructors at the owning inventory boundary.
Carry exact choices and edge targets without rescanning broader custody. Salvage
b1's direct requirement-issued binding reuse only with the sole-foreign refusal
and valid historical-child acceptance controls. Replace its permissive boolean
matching. Preserve all compatible custody alternatives instead of b3's deletion.

Repair normalization at its existing owner. Exercise newly issued and inherited
dependency edges which overlap another selected body. Preserve partial versus
Full permissions and explicit current Original conflicts. Compile affected test
targets and the service output consumer immediately after signature changes.

### N2: first publication and projected reopen

Salvage root E's narrow-before-capture idea only. Implement the required closure
and immutable birth/current-projection contract above. Its offer filtering must
retain required exact dependency custody without restoring unrelated roots.
Do not adopt its reopen re-narrowing or InterfaceOnly prose without checking the
actual distinct policy: that branch still keeps native rows for retained
compatible payloads. Tests must distinguish compiler role, availability and
executable demand explicitly.

Recheck birth acquisition identity, source hash, exact native edges and current
projection independently through live publication and recovery. Remove obsolete
branches after the replacement controls pass. N1 and N2 share compiler projection
files; the native owner serializes their edits and integrates before handing off.

### N3: completed issuance and World consumer

Join service-cells' authenticated completed-output owner to the repaired native
resolver and publication support. Fix A7 through the exact identity handoff
confirmed in N0. Admit accepted cached products only through that same completion
constructor; confirm A8 either receives genuine acknowledgement or remains a
truthful refusal with the product compiled by the normal service path.

The service-cells owner changes checked output/receipt constructors; this leaf
provides native policy commits and reviews the joined consumer. Runtime-publication
owns actor entrypoint cutover and deletion. Coordinate any cross-file change in
`artifacts.rs` / `certified_products.rs` before editing. Do not create a legacy
cell adapter to unblock tests. Join native repairs before claiming the World
successor capability executable; integrate signatures earlier for compile feedback.

### N4: joined acceptance and remaining failure disposition

Execute the first two imported-module cells through World, then declarations,
mixed cells and retained source-defined types. Resolve and rerun A1c through the
actual `haskell` tool; assert all three structured outcomes. Feed the earlier
Persona candidate gate in the parent plan as soon as its controls pass.

Run the affected native histories and current toolchain suite. Work with the
tests leaf to close each relevant member of the 32-red ledger through a repair
or obsolete-test disposition with its valuable invariant covered on World.
Do not merge Batch B/C weakened equality, permission, lifetime or work assertions.
Independent review checks full cumulative diff and actual World consumer before
engine integration acceptance.

## Tests and independent oracles

Use existing proptest/hosted infrastructure. A small independently authored raw
graph model records payload IDs, full issuing selection IDs, labeled parent/child
edges, current roles, custody, choices and demand as separate facts. Compute its
closure by plain graph traversal from explicit model roots; do not invoke the
production resolver, offer normalizer or snapshot writer to obtain expected
selections. Generate retain/reorder/narrow/publish/reopen/recovery histories and
shrink failures. Positive authority enters through real World/physical output
issuers; raw malformed facts are used only at local authentication boundaries.

| Property | Required accepting and refusing controls |
|---|---|
| Root choice | Exact carried binding accepted; absent/empty/unavailable/foreign-scope choice refuses even with one otherwise compatible candidate. |
| Historical edge | Foreign ancestor's actual issued child accepted; replacing its parent edge, carrier or selection ID refuses. Canonical equality cannot repair it. |
| Custody independence | Unrelated retention/reordering does not change roles or demand; alternative carriers survive narrowing and recovery; conflicting simultaneous consuming choices refuse. |
| Current completion | Actual produced identity and accepted cached identity accepted; pending/unbound original, equal reconstructed substitute, unacknowledged candidate and unrelated output promotion refuse. |
| Native modes | Full and partial issued groups retain exact permissions; canonical-only input does not create demand or Full; complete body qualification cannot union partial carriers. |
| Publication | First birth excludes unrelated owners but includes required empty-import/type/family/orphan/instance/native obligations; required withdrawal refuses, unrelated withdrawal succeeds; neither changes birth. |
| Reopen/recovery | Same birth with changed current projection succeeds; extra ambient inventory does not add roots; changed exact dependency or incompatible current Original refuses. |
| Ownership | Carried entries/materializations remain with their actual owner; drop predecessor/disposable output and reuse successor; no reconstruction or false release receipt. |

Demonstrate oracle sensitivity with bounded mutations: erase selection IDs,
choose sole foreign custody, change parent edge, promote custody to demand and
restore all birth roots into a current subset. Each must break its control.

Retain and migrate the useful existing cases, including
`independent_native_roots_keep_overlapping_exact_descendant_bindings`,
`native_variants_keep_exact_carriers_and_require_explicit_materialization_selection`,
`completed_request_carries_exact_entries_without_reconstruction`,
`fresh_candidate_dependency_uses_issued_availability_without_selecting_execution`,
`continuation_namespace_preserves_roles_without_promoting_retained_bodies`, and
`publication_reopening_refuses_incomplete_lexical_type_namespace`. Preserve their
behavioral contracts while replacing obsolete authority-forging fixture setup.
`merge_fixture_selections` alone cannot establish a production merge defect.

Run each selected existing or newly added case through the admitted pinned
entrypoint, for example:

```sh
swarm-build just test-lib tidepool-toolchain --exact FULL_TEST_NAME --expected-count 1
swarm-build just test-lib exomonad-host --exact actor_host::documentation_tests::published_introspection_example_returns_structured_declaration --expected-count 1
```

Prepare this revision's declared test/browser tools as required by the build
guide; use its supported local-only configuration. New test names must be listed
and execute a nonzero count. Compile directly affected targets and consumers,
run language formatting and `git diff --check`. Compiler serialization changes
also require the owning fixture checks. Full qualification belongs to the
qualification leaf and one exact bundle; focused native passes are not a release.

## Handoff and escalation

Each wave reports base/head, actual production entrypoint, changed constructors,
removed routes, exact commands, selected/executed counts, exit status and private
evidence location. Report compilation, execution and unresolved causes separately.
Old branch green counts do not qualify forward repairs.

Escalate with a minimal retained history if a real completion lacks enough
accepted-product/edge evidence, required instance closure cannot be derived from
issuing owners, or recovering birth would require borrowing another acquisition's
authority. Do not guess a new schema, restore a general issuer or broaden custody
into selection. The campaign owner resolves a contract change with the service
and native owners. Continue independent ready work while that question is open.
There are no deliberately deferred native semantics in this leaf; local type
names and implementation decomposition may vary without changing the contract.
