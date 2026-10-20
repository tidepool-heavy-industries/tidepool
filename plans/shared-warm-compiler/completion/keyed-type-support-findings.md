# Keyed World type support

`a16fb79556` adds the primary-only `RequestCompilerInputs::with_type_interfaces`
transition. It returns the updated request inputs, the actual updated declaration
context and changed owner keys. Empty or equal evidence retains the same input
Arc. Only the real cold World-primary capture retains the context owner; a generic
scaffold/private request cannot advance a primary. The context preserves its
producer, lexical facts, instance facts and template owner. Runtime authority is
not issued by this transition.

The existing metadata owner now uses persistent owner/artifact maps and a
persistent dependency set. The read projection shares the same dependency root;
there is no second retained dependency census. Unchanged native selection,
offers, decisions and original-source custody remain shared. Explicit complete
observation and wire encoders still materialize their ordered output.

`ebdf363494` adds direct ancestor IDs to the existing immutable scope fact owner.
The index is derived at owner birth, including decoding, and is not serialized.
Cold raw-proof installation and transfer of an already-issued reference use one
common validator, preserving proof-body conflicts and missing-ancestor refusal.
The missing-original-owner list is an immutable shared slice.

## Measured component controls

The lower keyed transition adds the same two closed canonical roles to retained
native support of different sizes:

| Unrelated roles | Incoming row visits | Keyed lookups | Retained-role census/resolution | Reference rows copied by persistent paths | Changed native-fact roles |
| --- | --- | --- | --- | --- | --- |
| 1 | 4 | 5 | 0 / 0 | 8 | 2 |
| 32 | 4 | 5 | 0 / 0 | 12 | 2 |
| 128 | 4 | 5 | 0 / 0 | 12 | 2 |

No canonical certificate hashing, descriptor reconstruction, complete metadata
read or native graph visit occurs in that measured transition. Structural path
reference copies are distinct from semantic row scans. The existing live-view
registry cleanup remains a residual linear scan; these fixtures observed two
registry entries. That scan releases strong snapshot references for dead views
and was not removed to improve a counter.

The narrowed upstream projector plus insertion was also measured against a
fixed two-row type closure from a wide incoming issuer:

| Incoming native groups | Ancestor-edge visits during issuer birth | Warm ancestor-edge visits | Selected type census | Old general projector graph visits |
| --- | --- | --- | --- | --- |
| 1 | 15 | 0 | 2 | 15 |
| 32 | 170 | 0 | 2 | 46 |
| 128 | 650 | 0 | 2 | 142 |

Those numbers do **not** establish acceptance of the narrowed projector: the
historical-custody regression below must be resolved first.

Build `b5165b55-12b7-465a-82fd-27418ef9df0d` compiled and executed six
focused controls, **6 passed, exit 0**, at the `ebdf363494` source content.
They cover the two scaling matrices, exact canonical-owner/binding refusals,
primary context coherence/equal Arcs, immutable semantic/display encoding and
independent complete metadata. Rust executable SHA-256:
`224c7a6e3b4acb41f7fe02beac25826141a5ac6815f763c2167f72abc419ebc1`.
The exact selections, commands and per-case reports are retained under
`/srv/swarm/evidence/inanna/keyed-types-owner-controls2-20261020`; the launcher
log has the same path plus `.log`.

## Confirmed wider failures

The affected-history tranche executed five tests: three passed, two failed,
exit 1. Build `16d7df15-0f6b-4f78-98eb-4442883181cd`; evidence:
`/srv/swarm/evidence/inanna/keyed-types-affected-histories-20261020.log`.

1. `view_read_properties::cached_read_projection_matches_exhaustive_graph_after_view_histories`
   preserves an intentional distinction: interface permission can omit native
   selection while retaining the exact original carrier as passive custody.
   The narrowed `support_for_owners` implementation discarded that custody.
   The minimal failure has all five dependency masks zero, seed 1 and no
   generated choices; shrinking seed
   `ea1eac6636ef59bdfa509b715969dbf0cdb3388530d3904e8b4ddd796ebecf62`.
   Reversing only `ebdf363494` restores this passing history. The assertion has
   not been removed or weakened. Source ownership/permission separation must be
   repaired before this optimization is accepted.
2. `compiler_projection_properties::compiler_domains_and_target_roots_match_independent_variant_histories`
   fails at its whole-original `early.unwrap()` with
   `NativeGroupUnavailable { original_ordinal: 3 }`. It also fails at exact
   `29ce1590e9`, before all keyed metadata, projector and scope-index changes.
   The deterministic history prelude fails even when the generated tail shrinks
   to repeated offers. This is an inherited native-history failure; its cause
   is not established here. The baseline comparison build is
   `110e3e80-17c3-4e02-8ad1-90344dabfb93`, executed 1, failed 1, exit 1;
   `/srv/swarm/evidence/inanna/keyed-types-before-metadata-20261020.log`.

Both comparisons used exact inverse patches in this isolated checkout, then
reapplied every patch and verified a clean tree. The patches and intermediate
comparison reports are retained in the same evidence directory. No main branch
or another worker's source was changed.

## Unqualified boundaries

The actual two/eight-shadow native cold-restore control passed before this
metadata tranche; its separate report is `native-world-lifetime-findings.md`.
It must be rerun after this tranche's custody semantics settle. The runtime
consumer and real retained-value two-cell execution belong to the engine owner.

The component refusal controls use existing certified representation fixtures.
They do not establish the additional premise that two genuine authored source
products produce equal checked signature bytes but different type-interface
owners. That actual producer control remains open with the query/engine owners.
No alternate trusted signature constructor or fabricated World/finite request
was introduced to satisfy it.
