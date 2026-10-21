# C1 schema-and-equivalence report: the four TPGRP-v2 envelopes

Role C1. Checkout `tidepool-deepseek-g2-c1-20261021`, branch
`g2-deepseek/c1-20261021`, base `0cfb964739c9776f602a9581d323bf8e4922dd0e`.
Read-only investigation; no production edits, no campaign merge. Not a release
prerequisite.

## Verdict in one paragraph

Each pair encodes the same executable meaning: field 14 (bindings) is
byte-identical, field 13 (frames) is identical after the constructor table is
renumbered through the compiler-issued `host_id`, and field 17 (constructor
replies) points at the same two constructors and the same `Root
'ConstructorScheme' [1] 'a'` answer type after the same renumbering. The
divergence is confined to fields 11 (constructors) and 15 (types): one side of
each pair carries the complete constructor table (38 entries: the three
list/Char constructors plus 35 representation-metadata constructors) and the
full type graph, while the other carries the minimal table (3 entries) and a
two-node type graph. The minimal side is an exact subgraph of the complete side,
so the two are "same executable program, different retained metadata", not two
different programs. Full type-graph equivalence (i.e. that the extra nodes are
unnecessary) remains unproved and is the C2 demand question, not a byte-level
fact.

## Method

The schema reference is the real Haskell encoder
`bridge/haskell/execution-schema/Tidepool/ExecutionEncode.hs`
(`encodeProjectedGroup`, `encodeConstructor`, `encodeExprFrame`,
`encodeHeapRhs`, `encodeTypeGraph`, `encodeConstructorReply`) and the real Rust
decoder `tidepool/repr/src/execution_schema/codec.rs`
(`decode_group_wire`, `definitions`, `type_graph`). They agree on every tag
number; the comparison below was produced by a diagnostic transcription of
that grammar, not by a generic CBOR diff and not by a second production
decoder.

Artifacts (private to inanna):

- `/srv/swarm/evidence/inanna/g2-closeout-deepseek-20261021/c1-schema/tpgrp_typed.py`
  — typed transcription of `decode_group_wire`.
- `.../c1-schema/compare_typed.py` — renumbering-aware comparison and negative
  control.
- `.../c1-schema/typed-dump.json`, `.../c1-schema/comparison.json` — outputs.

The retained envelopes were read by full filename (each filename is its own
SHA-256):

- Pair 1, `ghc-prim:GHC.CString.unpackCString#`:
  `1f79dd87206bbc08214753a81c95640716af7fe4a5f37e6136e34fd511791831`
  (38 constructors) vs
  `ee13b574bd96c502c120c40e4f781135b70fa403435bf6523fd47c75c071e834`
  (3 constructors).
- Pair 2, `text-2.1.2-f933:Data.Text.Internal.$wouter`:
  `60ce18cc7d6597a7c9d722cd3f67e6a075dd2cb18b9527df3a92291f004198ef`
  (3 constructors) vs
  `b368ec8ef4902789555bb86f675776e6e87a41f24bcb6a65ba33147996227f8e`
  (38 constructors).

`constructorHostId` (Rust `ConstructorDecl.host_id`) is the identity used for
renumbering. It is stable across table order: wired-in constructors use the GHC
work-id unique; others use `0xFE << 56 | first 7 bytes of SHA256(canonical
identity)` (`bridge/haskell/src/Tidepool/ConstructorIdentity.hs`). The three
core constructors carry identical `host_id`s in all four envelopes:
`C#` `18320975647865051484`, `:` `18374311222611660222`,
`[]` `18329591212420036359`.

## Differing-field / identity table

| Field (index) | Producer | Consumer | Identity vs index | Observed difference |
|---|---|---|---|---|
| 0 magic, 1 version, 2 ordinal, 3 binders, 4 schema, 5 profile, 6 toolchain, 7 abi, 8 target, 9 signatures, 10 globals, 12 operations, 16 sites, 18 json_layout | `encodeProjectedGroup` | `decode_group_wire` / `definitions` | none renumbered | equal in both pairs |
| 11 constructors | `projectedConstructors` (`constructorDecls`) | `ProgramDefinitions.constructors` | table index is local; `ConstructorIdentity` (symbol + family + version) and `host_id` are nominal/stable | 38 vs 3 entries; the 3 core entries are fully equal by identity + layout; 35 extra entries are representation metadata |
| 13 frames | `encodeExprFrame` | `expr` | `ConstructorId` refs in `Construct`, `Case` constructor patterns, and `Let` constructor RHS are table indices | raw diffs at slots 4,5,7 (pair 1) and 180,184 (pair 2); all equal after `host_id` renumbering |
| 15 types | `encodeTypeGraph` | `type_graph` + `TypeGraph::validate_with_work` | `TypeNodeId` edges; `ConTemplate`/`TypeEdge::Constructor` refs are table indices | minimal side (2 nodes, 1 edge) is an exact subgraph of the complete side (63/92 nodes) |
| 17 constructor_replies | `encodeConstructorReply` | `constructor_replies` | `ConstructorId` + target `TypeNodeId` | same two constructor hosts; same `Root 'a'` target after node renumbering |

## Constructor identity/layout finding

The 38-entry table is **not** a disjoint replacement of the 3-entry table. The
three entries present in both are byte-for-byte equal in every semantic field:
constructor identity (symbol + family + version), field representations,
strictness, checked layout (field offsets, alignment, payload size, root mask),
result representation, tag, family size, and host id.

The 35 extra entries fall into exactly four wired-in representation families:

| Family | Count | Members |
|---|---|---|
| `GHC.Types.VecCount` | 6 | Vec2, Vec4, Vec8, Vec16, Vec32, Vec64 |
| `GHC.Types.VecElem` | 10 | Int8ElemRep, Int16ElemRep, Int32ElemRep, Int64ElemRep, Word8ElemRep, Word16ElemRep, Word32ElemRep, Word64ElemRep, FloatElemRep, DoubleElemRep |
| `GHC.Types.Levity` | 2 | Lifted, Unlifted |
| `GHC.Types.RuntimeRep` | 17 | VecRep, TupleRep, SumRep, BoxedRep, IntRep, Int8Rep, Int16Rep, Int32Rep, Int64Rep, WordRep, Word8Rep, Word16Rep, Word32Rep, Word64Rep, AddrRep, FloatRep, DoubleRep |

The list constructors (`[]`, `:`) and the Char constructor (`C#`) are the three
core entries, not extra metadata.

## Type graph and reply finding

The minimal side's type graph is exactly `{Root 'ConstructorScheme' [1] 'a',
Bound 0}` with the single `Body` edge `Root -> Bound`. Those two nodes and that
edge appear verbatim in the complete side (after the node-index rebase), so the
minimal graph is a subgraph, not a different graph.

The complete side adds: a `ConTemplate` for all 38 constructors; `Declaration`
nodes for `List`, `TYPE`, `RuntimeRep`, `VecCount`, `VecElem`, `Levity`,
`BoxedRep`, `Lifted`, `Char`, `Char#` (pair 1) plus `Many`, `MutableByteArray#`,
`Int#`, `State#`, `Tuple4#`, `TupleRep`, `[]`, `Unlifted`, `IntRep`,
`ByteArray#` (pair 2); `NominalApplication`/`Application`/`Function`/`ForAll`/
`Bound` scaffolding; and one extra `Root 'Closed'` carrying the rendered result
type (`"[Char]"` for pair 1, the full `forall s. MutableByteArray# s -> ...
-> String -> ...` type for pair 2). Pair 1 adds 61 nodes / 67 edges; pair 2
adds 90 nodes / 122 edges.

Replies: both sides carry two entries for the same constructor hosts (`[]` and
`:`), and both point `Static` at the same `Root 'a'` answer type after node
renumbering (complete-side node 62/91 == minimal-side node 1). The extra
`Root 'Closed'` result type is **not** referenced by any reply entry.

## Negative control (the comparison is not vacuous)

Four deliberate corruptions were checked so the comparison cannot pronounce
every shape equivalent; all four are detected as inequality in both pairs:

- wrong constructor: `:` field reps changed to `[Int64, Int64]`;
- wrong layout: `:` payload size incremented;
- wrong type identity: `:` family module changed to `GHC.Tuple`;
- wrong frame reference: a `Construct`/`Case` reference to `:` re-pointed at
  `C#` (a constructor that exists in both tables but is not the one the frame
  meant).

This confirms the equality predicate is sensitive to real semantic content and
only cancels the documented constructor-table renumbering.

## Incidental findings: retention, decode, order

These are schema-level observations with source locations; the demand trace is
C2's, so I do not conclude they are removable.

- Redundant retention: the complete side retains 35 representation constructors
  and the full type-graph expansion for a binding whose executable frames
  reference only `C#`, `:`, `[]`. The producer unions three graph sources
  (`moduleNodes`, auxiliary roots, intrinsic constructor replies) in
  `lowerPreparedEvidence` and interns every constructor reachable from the
  merged type graph (`bridge/haskell/src/Tidepool/ExecutionProjection.hs`).
- Decode: `decode_group_wire` -> `definitions` fully decodes and validates the
  type graph (`TypeGraph::validate_with_work`) for every group regardless of
  whether any consumer reads the type/reply data
  (`tidepool/repr/src/execution_schema/codec.rs`).
- Order: the constructor table order is not canonical across the two encodings.
  The minimal table orders `[C#, :, []]`; the complete table orders by family
  (`VecCount*`, `VecElem*`, `Levity*`, `RuntimeRep*`, then `[]`, `:`, `C#`).
  This ordering difference, not the renumbering itself, is what keeps the two
  byte encodings distinct and defeats byte-level dedup.

Multiplicity, sharing, recursion and nominal identity are preserved by the
comparison: the type graph is compared as nodes plus edges (sharing and
recursion intact), `family_size`/`tag`/`host_id` are compared as nominal facts,
and constructor identity is compared by symbol + family + version, never by
rendered text or numeric node ID.

## Confirmed vs hypotheses

Confirmed (typed byte-level comparison):

1. Two owner-equal pairs as listed above.
2. Bindings (field 14) byte-equal in each pair.
3. Frames (field 13) equal after `host_id` renumbering; raw diffs at slots
   `[4,5,7]` and `[180,184]`.
4. Three core constructors fully equal in identity and layout across all four
   envelopes; 35 extra entries are exactly the four representation families.
5. Minimal type graph is an exact subgraph of the complete type graph.
6. Reply constructor hosts and reply target root are equal after renumbering.
7. Negative control detects all four deliberate corruptions.

Hypotheses / deliberately not claimed:

- Complete code or whole-program equivalence is **not** claimed from the equal
  binding rows or the frame renumbering alone.
- "The extra type nodes are unnecessary" is **not** proven; that depends on a
  consumer (C2's demand trace).
- "The two byte variants may be deduplicated" is supported by bindings, frames
  and replies but is only safe if no consumer reads the complete-side type
  graph; left as a proposal, not a fact.

## Leave-semantics alternative vs owner-level demand cut

Option A (leave semantics unchanged): canonicalize the byte encoding (stable
constructor order and/or a semantic dedup key that ignores renumbering and the
extra type metadata) so the two variants collapse to one retained product. This
changes no executable semantics because bindings, frames and replies are equal;
it only risks losing the extra type graph, so it is correct only if nothing
consumes that graph. It does not remove the producer work.

Option B (owner-level demand cut, C2): stop emitting the representation
constructors and full type graph where no consumer needs them. This removes the
work at its earliest owner and is the larger, producer-changing option; it must
be justified by the demand trace plus executed GHC/runtime controls, not by this
byte-level report.

## Remaining evidence

- C2's demand trace for `selectPreparedEvidence`, `lowerPreparedEvidence`,
  auxiliary roots, and type/reply lowering in `ExecutionProjection`, plus the
  actual Rust/runtime consumers of the type graph and replies.
- Whether any consumer reads the extra `Root 'Closed'` result-type rendering or
  the representation ladder; until then type-graph necessity is open.
- A canonical-order/encoding experiment and its measured byte-size saving on the
  same workload, before any producer cut.

## Reproduction

```sh
cd /srv/swarm/evidence/inanna/g2-closeout-deepseek-20261021/c1-schema
python3 tpgrp_typed.py    # -> typed-dump.json
python3 compare_typed.py  # -> comparison.json (includes negative control)
```

Script SHAs: `tpgrp_typed.py`
`82e3e1a19ed915e7eb480ccf05f5c9d76302aab94839d60186fd132da6d3c259`,
`compare_typed.py`
`f54fc64ce7243bf65857d6f0ea7005c8b75065ed9ab6c010bf704d26a3919405`.
