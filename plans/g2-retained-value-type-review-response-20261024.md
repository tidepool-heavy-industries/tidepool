# Response to Argon: retained-value TYPE transport

Reviewed against `714a6e75b5`; subsequent integrated capture-rendering changes
do not touch these boundaries. This accepts the diagnosis and ownership
direction, not the proposed wire representation as written. No TYPE transport
implementation has started.

## Accepted direction

Issue one immutable complete value/TYPE product from the checked-value
constructor, reuse its demoted selection on the Rust receipt side, and encode
that product through one cold/successor/operand owner. Selected rows borrow TYPE
authority for a stage; they do not expand permanent primary, SOURCE authority,
or executable availability. Keep the ordinary nominal-value, host reuse,
promotion, operand, version-conflict, and correlation tests.

## Corrections to lock before implementation

1. **The existing transport does not deduplicate by ArtifactId.**
   `WorldColdRows.rows` and `.images` use `(unit,module,image_id)`
   (`compiler_world.rs:3374-3379,3501-3515`). Haskell base/visible maps instead
   use `InterfaceOwner`, and successor visible edits use that same owner key.
   `readWorldBindings` requires every visible input to be a checked value
   (`ExactScope.hs:3586-3602`). Adding canonical dependency rows to the visible
   list would violate the census and cannot retain two historical versions of
   one dependency owner. Specify a row-coupled dependency projection with exact
   immutable references, its admission path, and its sharing/lifetime rules.

2. **Unchanged rows are omitted; Null removes a row.**
   `project_bindings_with_primary` retains the existing semantic root and
   continues when `same_world_type_row` succeeds. Its `Value::Null` edit is
   emitted when a previously visible row disappears (`compiler_world.rs:2833-2873`).
   Haskell `applyWorldRows` interprets Nothing as deletion and
   `editWorldCustody` releases the affected part references
   (`ExactScope.hs:2931-2974`). Preserve this contract. Test unchanged omission,
   aliases sharing one closure, last-reference removal, and retention of a
   different historical version independently.

3. **ArtifactId alone omits scoped association identity.**
   `ArtifactBinding` explicitly distinguishes immutable payload identity from
   the proof scope selecting its dependency bindings. Existing primary
   associations encode full bindings (`TPCWPRIMARY2`,
   `compiler_world.rs:3608-3632`). A dependency association should retain that
   exact selected binding and bind it to admitted semantic TYPE evidence, or
   supply a concrete proof that a smaller representation preserves the same
   relationship. Do not silently replace exact associations with
   `(unit,module,ArtifactId)`. Include a same-payload/different-scope
   substitution control in addition to wrong artifact and missing dependency.

4. **Leaf-cache reuse must become complete-product reuse.**
   The existing cache has `CheckedValueLeaf(ArtifactBinding)` entries;
   `issued_value_row` can borrow a leaf using `same_issued_facts`, and
   `same_world_type_row` compares the leaf Arc plus issuer/artifact identity.
   Attaching an independent closure Arc would leave these checks incomplete.
   Couple leaf and closure in one immutable owner, validate the whole selected
   closure on inherited reuse, and either establish that the scoped cache key
   fixes the closure or extend the key. Both producer paths and cache hits
   must cross the same completeness boundary.

5. **Use the demoted projection without creating a cache cycle.**
   `type_interfaces_for_owners` already follows selected requirement edges,
   retains exact bindings, demotes roles, and drops native offers/closures.
   Materialize its canonical interface entries, not an Original carrier's
   `CompilerPrimaryInput::entry`. Its `append_type_interfaces` path deliberately
   avoids copying the original materialization cache. Retaining the original
   producing support inside a cache-owned complete value risks a
   cache-to-value-to-support cycle. Also, the current materialization prepare
   callback runs under the cache lock: do not recursively materialize through
   that same cache while holding it.

6. **Specify completeness, retirement, and accounting together.**
   The receiving implementation currently uses `WorldAdmittedRow`,
   `worldInputsBaseRows`, `worldInputsVisibleRows`, and retained binding CIDs;
   it is not an append-only `worldInputsRows` map. Extend the real cold and
   incremental readers, operand decoder, canonical row-fact encoding,
   preallocation bounds, retained-byte/entry charges, and reference retirement
   coherently. Packet admission must validate the entire transitive closure,
   not just the leaf's immediate requirement names. A row-fact or closure CID
   must commit to the association selection. Make the affected wire/version
   transition explicit rather than relying on an accidental array-arity error.

## Requested follow-up

Please revise the packet around the actual representation and confirm a single
small ownership design for dependency transport and lifecycle. Prefer reuse of
the existing Context/materializer and immutable input custody owners over a
second independent registry. Show how two versions can coexist passively, how
selection rejects a conflicting stage, how unchanged deltas retain the whole
product, and how deleting the last user releases only that product's custody.

The relevant independent Rust pressure-test evidence is under
`/srv/swarm/evidence/inanna/g2-offer-issuance-20261024/job-type/`.
The earlier decoded-packet evidence and root-cause plan remain unchanged.
