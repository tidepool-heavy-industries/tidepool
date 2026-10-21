# C investigation: avoid metadata nobody consumes

Two independent investigators work in parallel, initially read-only. C1 maps
actual encoded meaning; C2 traces demand to consumers. Their joined result is a
bounded implementation proposal, not an automatic rewrite or a G2 prerequisite.
Reuse their source context for secondary findings on duplicate work, unnecessary
retention and old APIs; distinguish findings from conjecture.

## Corrected starting evidence

Four real package envelopes form two differing pairs:

- `1f79dd8720…` versus `ee13b574bd…`, GHC.CString `unpackCString#`.
- `60ce18cc7d…` versus `b368ec8ef4…`, Data.Text.Internal `$wouter`.

The evidence script checks full retained filenames/hashes; abbreviated digests
here are locators, not authority. Run the bounded inspection with:

```sh
swarm-command python3 /srv/swarm/evidence/inanna/g2-closeout-deepseek-20261021/compare-package-products.py
```

TPGRP-v2 has nineteen fields. Constructors are 11, expression frames 13,
**bindings 14**, types 15, sites 16, constructor replies 17 and JSON layout 18.
The older comparison confused types with bindings. The actual binding rows are
equal in both pairs. Differing frame slots refer to equivalent complete
constructor declarations after local renumbering. The 38-entry table includes
the three list/Char constructors plus representation metadata; it is not a
disjoint replacement. Type/reply equivalence remains unproved.

## C1: schema and equivalence

Use the actual Haskell encoder and Rust decoder as schema references. Compare
constructor identity/layout, frame references, type graph, typed sites, reply
and rendering evidence with locations and explicit unknowns. Do not compare
pretty text or numeric node IDs as semantic identity. Preserve multiplicity,
sharing, recursion and nominal module/constructor distinctions where relevant.

Provide a small independently checkable table: differing field, producer,
consumer, identity/index interpretation, semantic difference or renumbering,
and evidence still needed. Check a deliberately wrong constructor/layout/type
so the comparison cannot pronounce every shape equivalent. Reuse existing
typed decoding rather than build a second general package decoder casually.

Do not select an arbitrary package product, drop type/reply evidence, or infer
complete code equivalence from the equal binding rows alone.

## C2: earliest demand and downstream use

Trace `selectPreparedEvidence`, `lowerPreparedEvidence`, auxiliary roots and
type/reply lowering in ExecutionProjection, then their actual Rust/runtime
consumers. Explain why kind/RuntimeRep metadata enters the package product and
whether each part is required for execution, rendering, contract validation or
only an unrelated root's support.

For each candidate cut record: original requester, data carried at each stage,
actual consuming query, invariant, unnecessary work and the earliest owner that
can avoid it. Consider making demands more precise before building a graph;
avoid filtering a fully built graph after paying the cost. Keep the paired
product and exact occurrence changes from P intact.

Don't make all package modules complete or compile whole packages merely to
stabilize hashes. Don't add a memo/cache to hide repeated irrelevant production.
Inspect canonical ordering only after identifying which facts actually belong.

## Joined proposal and cutoff

Return a large enough report to be useful: source map, counterexamples, demand
table, incidental findings, proposed deletion/API change, invariants, meaningful
controls, expected structural saving and what remains unmeasured. Compare at
least leaving semantics unchanged with an owner-level demand cut; reject any
option that trades correctness or maintenance complexity for apparent speed.

S may approve one small repair before B qualification freezes if C1/C2 agree on
the contract and an independent reviewer agrees on the oracle. That implementation
must run actual GHC/runtime controls and relevant fixture checks, then measure
packaged production preparation and output size on the same workload. Otherwise
commit/push the proposal separately and leave it ready for the next wave.

Even if this collapses the observed duplicate byte variants, P's exact occurrence
linkage remains necessary. Metadata simplification is not permission to restore
symbol/seal selection or to choose a different historical child.
