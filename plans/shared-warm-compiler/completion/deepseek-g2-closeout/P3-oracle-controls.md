# P3: current coverage and required oracles

> Historical assignment, superseded at the 5c759554 review. Use [NEXT-WAVE.md](NEXT-WAVE.md) and STATE for current work. Preserve useful contracts below; do not launch this old task list unchanged.

This replaces the earlier coverage claims after staff review of c910d1aae0.
The prior implementation/evidence remain in Git; the controlling assignment is
[P-package-linkage](P-package-linkage.md). The detailed audit is
/srv/swarm/evidence/inanna/g2-peer-review-c910d1-20261021/tests-quality-review.md.

## What the six passing cases actually cover

| Existing case | Supported claim | Missing discriminator |
|---|---|---|
| reversed_targets_keep_exact_ordered_package_slots | Ordinary target-order consistency | Actual competing same-seal bodies and issuer-selected edges |
| transitive_import_prefers_exact_child_over_competing_sibling | Nonempty/shared-symbol smoke where matches exist | A named transitive edge with two demonstrated distinct candidates |
| repeated_slots_share_immutable_bytes_without_losing_multiplicity | GHC physical-global dedup | Repeated wire root/edge cardinality and shared custody |
| duplicate_envelope_histories_remain_distinct_bindings | Two nonempty result lists | Equal payload with genuinely different exact child histories |
| multi_ordinal_home_keeps_per_ordinal_package_roots | Union contains two package owners | Each actual ordinal's ordered selected roots and absent wrong root |
| historical_same_seal_bodies_select_separately_and_refuse_seal_symbol_match | Authentic frozen restore, individual graph issue and old selector refusal | New occurrence reader, target/item association and graph reopen |

The first five use ordinary compile_invocation; none exercises World
segment-item publication. The historical case's singleton plan path bypasses
the new linkage reader. Its captured products have zero imports, so they
cannot establish transitive/different-child-history behavior.

## Repair direction

Keep useful smoke assertions under accurate names; delete duplicates rather
than preserving the old count as a goal. The owning counted target must reflect
actual nonzero case discovery/execution after changes.

P3 first identifies real producer premises for competing target/child scenarios:
source/history, exact selected coordinates, equal digest/seal where claimed,
distinct implementation/edge where claimed. Do not use different reachability
as proof that a shared per-binding product changed. Unavailable premise means
unqualified construction, to escalate to P/R-P; not permission to manufacture
authority or weaken assertions.

The earliest joined gates are a package-using World item and repeated Fresh
HOME across two ordinary targets. Add exact per-ordinal roots, unused competing
custody, legal identical-content occurrences, repeated ordered roots/edges,
and typed invalid-coordinate controls as described in P.

Delete the lossy depth-eight history observation. For bounded histories assert
the concrete issued root/child identities and edge positions; a reused general
observation must preserve finite graph structure and cycles. Assert list
lengths before zipping.

Raw syntax/refusal tests may mutate transport. Positive current issuance must
come from the compiler/output owner and real admission, never a test-only
certificate constructor. Report parser, component-owner and end-to-end coverage
separately.

## Fixture and API retirement

Q owns the narrow frozen-reader provenance policy; P3 owns its honest test
claim. Keep the historical counterexample but retarget it to versioned restore/
identity/hash/provenance when the obsolete held selector is removed. A test
must not keep a deleted production API alive.

Remove canonicalizer tests when its unnecessary quotient policy is removed;
replace them with actual coordinate/payload/multiplicity controls. Distinct row
IDs with equal contents are legal; duplicate IDs are not.

The real historical reader still needs exact old/current edges, semantic
results, disposable-output removal and cold/eviction evidence. Do not call a
new ordinary smoke pass completion of that gate.

## Repair checkpoint controls

The `g2-repair/p3-20261021` checkpoint replaces the lossy observations above
with:

- finite issued-graph records keyed by exact `ArtifactBinding`, preserving
  ordered edges and cycles without a depth cutoff;
- exact HOME-root cardinality, issued ordinal, exported fixture binder and
  cross-ordinal absence;
- a repeated-Fresh HOME history with `result` and `result2`, including reversed
  target order and exact prior-binding reuse;
- a real package-using World item followed by a later cell consuming its
  captured value;
- a real direct identity/child-history pair from `sortInts` and `sortStrings`.
  The direct package owner, interface, envelope and body agree while the
  ordered child bindings differ.

The first truthful run executes seven passes and two failures: the World item
stops at `target package slot lacks package links`, and repeated Fresh HOME
stops at `finite targets disagree on a physical native product`. These are
producer/consumer construction failures, not oracle failures.

Crossing the serialized program/package pair for refusal and adding a second
compatible inherited graph remain unqualified until P exposes the
receipt-issued `BoundPackageSelection` constructor. No test-only constructor is
introduced to bypass that dependency. A scanned set of current ordinary
receipts found no equal-envelope rows with distinct non-empty import histories;
that relationship remains explicitly unqualified.
