# Response to Argon: retained-value TYPE transport

Reviewed against `9e09843fcd`. The coupled product, separate scoped dependency
custody, omission/deletion distinction, and selected-only TYPE borrowing are the
right direction. The revised packet is not implementation-locked yet: several
claimed existing schemas and invariants differ from this source. No TYPE
transport implementation has started. This is a bounded follow-up.

## 1. Establish the cache key's exact closure invariant

`ScopeProofV3` authenticates exact dependency edges, so a leaf `ArtifactBinding`
can anchor its closure. However, `SelectedCompilerInterfaces::project_interfaces`
in `artifact_inventory/compiler_projection.rs:2510-2577` currently traverses
`entry.requirements` and `self.get(owner)`. Its requirement validation checks
owner presence and interface seals, not equality with the leaf graph's exact
child bindings (`6890-6933`). Merely calling that projection does not establish
the proposed scoped cache-key invariant.

At the coupled product's constructor, follow or correlate the selected rows
with the leaf's authenticated `ArtifactDependency::Interface` edges, recursively.
Do not resolve historical children through an unrelated ambient owner map.
Reject conflicting selections within that single closure. Keep native edges
and SOURCE/native permission outside this projection. Only after that proof
may `CheckedValueLeaf(leaf_binding)` stand for the complete product.

Cache reuse must compare the stored product with the **expected** closure:
exact key set, owner, expected entry facts, row-to-expected-entry correspondence,
and leaf binding/entry correspondence. Checking each cached row against its
own cached entry only proves internal consistency. Keep the product opaque and
issued through one constructor. Sharing physical rows across scope identities
does not equate bindings.

## 2. TYPE-only is not synonymous with Canonical-only

The current projection accepts both `ArtifactPayload::Canonical` and
`ArtifactPayload::Interface` (`compiler_projection.rs:6550-6580`). The latter
includes checked values and lexical joins. `readWorldTypeInputs` deliberately
handles `CheckedValueEvidence` and `LexicalJoinEvidence`
(`ExactScope.hs:2450-2505`). Native-declaration evidence is represented by
`ParsedModuleEvidence` with its role, not `ParsedNativeDeclarationEvidence`.

Direct value requirements are nominal owners, but canonical module requirements
include GHC `mi_usages` (`FinalizedModule.hs:21-35`). A declaration can define a
nominal type while capturing a checked operand; see the production fixture
`exomonad/actor/src/fixtures/world-declaration-captured-input.hs` and its consumer
at `resident_workbench.rs:21249`. No canonical-only transitive invariant has been
established. This review did not execute that closure as a new reproducer.

Preserve supported thin interface evidence roles in the dependency product, or
prove a narrower production invariant before rejecting them. Identify the
value leaf by its exact binding, not by dropping every ValueInterface from the
dependency list. A dependency-only checked interface must not create a visible
runtime binding or gain native authority. Add a nominal declaration with a
captured-value dependency to the discriminating tests.

## 3. Use the actual wire and custody owners

| Existing record | Actual shape at the reviewed source |
| --- | --- |
| `TPCWORLD` v5 | 11 fields; nested base/visible rows plus one `InputAcquisition` |
| `TPCWSUCC5` | 6 fields; restoration, blocks, primary edits, visible edits, acquisition |
| `TPCWREST` v5 | 10 fields; semantic roots/source/producer/profile/scope commitment |
| `TPCWCELLOPERAND1` | 6 fields; exposure, reference, blocks, offered row, acquisition |
| Binding row fact | 3-field canonical array: semantic row, certified artifact, receipt |
| `WorldArtifactBinding` | Existing `(ByteString, ByteString)` pair |

See `compiler_world.rs:649,2947,3266,4060` and
`ExactScope.hs:2380,2890,2908,3575,3773,3795`.

Extend these real encoders/decoders and explicitly version the changed contracts.
Keep physical dependency images in the existing `OriginalInputArenaTable` /
`InputAcquisition` transport, admitted as `OfferedWorldRow` evidence. Do not add
a parallel raw-image byte registry. `TPCWREST` must stay path/transport independent:
the binding-row CID can commit to the closure manifest, and the existing binding
root then commits to it transitively. A restoration version bump may be useful
to reject the old contract; physical image fields do not belong there.

The proposed selectors/readers are new design names, not existing source.
Actual paths include `readWorldBindingInputs`, `readWorldSelectedBindingInputs`,
`readWorldTypeInputs`, `readWorldCellOperandWithOwner`, `readWorldBindings`, and
`readChangedWorldBinding`. Preserve their package and exact-interface checks.

## 4. Keep lifecycle state coupled and incremental

A scoped dependency inventory with shared file custody is justified. Put the
closure manifest on the existing shared `WorldBindingRow` (or one opaque child
of it), whose CID already owns provenance and whose references are maintained by
`applyWorldBindings`. Avoid duplicating the whole manifest in visible rows,
`worldInputsVisibleClosures`, and every binding row without an owner-level need.
Likewise, prefer a dependency entry that couples admitted row and positive use
count over separate public maps of rows and counts. Define whether the count is
per live row CID or per distinct visible value product; do not count aliases
accidentally or mix units between cold and successor paths.

Existing incremental validation inspects touched aliases/row CIDs/semantics
(`validateChangedVisibleRows`, `ExactScope.hs:3020`). Do not recompute the full
dependency frequency map, transitive closure census, or every unchanged value
on each successor. Use changed products to derive net reference deltas, so a
dependency removed and added in one transition has no transient retirement or
duplicate wire edits. Keep full recomputation as an independent test oracle.

Integrate charges into `worldInputsRetainedCharge`, `validateWorldRetainedCharge`,
`checkedWorldCharge`, and Rust's preallocation/capture allowance. The proposed
`maxWorldRetainedEntries=4096`, `maxWorldRetainedBytes=64 MiB`, and corresponding
World helpers do not exist here. Current World collection bounds use encoded
payload size; retained backing uses `requestCaptureByteLimit`, with host-range
checks for counts (`ExactScope.hs:2593,2781-2806,3479`). Do not restore removed
arbitrary whole-World limits. Charge new indexes/manifests as well as file bodies.

## 5. Preserve exact selection and distinguish it from byte equality

`CompilerSourceSupport::with_type_interfaces` refuses different ArtifactIds for
one owner and also refuses equal ArtifactIds with different scoped bindings
(`compiler_projection.rs:4390-4406`). The proposed Haskell merge compares only
`ScopeInterfaceInput`, which can lose that distinction; its path fields can
also reject harmless physical aliases. Check exact selected bindings plus
semantic interface facts, and reuse admitted custody for equivalent physical
aliases. Follow the Rust selection contract rather than inventing a second one.

Historical versions may coexist in passive custody. Only demanded values and
the included primary/stage selections participate in conflict checks. The
positive old-version test must keep the new version out of conflicting primary
or stage selection; if primary already selects the new version, the existing
Rust boundary intentionally refuses the old dependency.

Haskell does not independently verify `ScopeProofV3` today. The comments at
`ExactScope.hs:3337-3345` explicitly retain Rust-issued binding provenance while
authenticating physical TYPE rows. State tamper tests precisely: changing a
binding against an unchanged committed manifest/root or retained association
must fail. A coherent rewrite with recomputed roots is a different logical
product; a table lookup alone cannot authenticate its claimed Rust issuer.
Keep issuance proof in Rust and request/root correlation at the receiving owner.

## 6. Finish the bounded cache and test contracts

The proposed raw row product avoids the producing-cache retention cycle, and
the demoted support can remain separate. Existing inherited reuse holds the
receiver cache lock while briefly locking ancestors
(`artifact_inventory.rs:7516-7536,6759-6815`); that is non-recursive receiver
access, not one cache lock total. Preserve the established acyclic ordering and
cancellation behavior, or snapshot inherited immutable handles before taking
the receiver lock and recheck its entry afterward. Never call materialization
through the receiver recursively from its prepare callback.

Keep the cold/successor/operand/recovery, historical conflict, shared-custody,
and complete-cache controls. Add alias multiplicity, same-transition replacement
with shared dependencies, equivalent physical aliases, exact-scope substitution,
and the non-canonical thin dependency case. Test valid products built through
production constructors; malformed-cache controls belong inside the private
owning test module. Record the graph correlation and transport/identity limits
explicitly before declaring this packet locked.

Independent source review:
`/srv/swarm/evidence/inanna/g2-offer-issuance-20261024/job-type/product-identity-pressuretest-9e09843fcd.md`.
No new build or test execution is claimed for this design review.
