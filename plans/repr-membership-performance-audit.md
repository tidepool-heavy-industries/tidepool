# Prepared decoding membership audit

Source audit: `5b738251ecf25ba6e75f002c02cc47da8f596eef`. Implementation
base: `b7683d9d65550ae41584cb975749f70323e0a306`.

The workflow is worker module-product bytes -> bounded CBOR grammar -> decoded
original group -> semantic validation -> certificate admission. Complete
prepared programs use the same site and verb-site validator. The successful
prepared/group types remain unpublished until all checks complete.

Two structural findings occur inside this existing admission owner:

- `execution_schema::codec::decode_group_wire` built an owned set of
  `(SymbolIdentity, ValueId)` and scanned it for each advertised binder.
  With B binders and T tops, membership took O(B*T) comparisons. It now builds
  a borrowed exact-identity set and uses membership lookup: O((T+B)*log T).
  Temporary per-group vectors and cloned identity strings are removed.
- `validation::Validator::check_verb_sites` scanned all S site rows for each
  of V verb declarations, despite `check_sites` having already built an exact
  ID set to reject duplicate sites. The validator now passes that validated
  set directly to verb checks. Membership changes from O(V*S) to O(V*log S)
  without another index construction or an index retained beyond validation.

These are structural savings, not measured end-to-end speedups. Large generated
groups and effect surfaces can pay the old comparison cost with valid trusted
input; malformed inputs could also consume that work beneath the wire budget.
No observed compiler/display slowdown is attributed to these checks.

The existing byte, table, node and charged-work budgets are unchanged. Identity
matching still includes unit, module, namespace, occurrence and record parent.
Binder membership still precedes semantic validation; duplicate binder
rejection still follows it. Site validation still precedes verb validation;
missing constructor, duplicate constructor, dynamic site and missing row checks
keep their order and typed errors. No wire schema, certificate, cache, registry,
package catalog or recovery format changes.

The focused fixtures admit 4,096 exact binders/tops and 4,096 synthetic site/verb
pairs. The former old sorted membership scans would perform 8,390,656
comparisons in these complete matching inventories. The fixtures verify
admitted counts, identity mismatch, duplicate binder/site rejection, table/work
bounds, and the exact existing one-unit-per-verb work charge. Existing tests
retain constructor, missing-row, dynamic-site and certificate owner refusals.

Validation commands, executed counts, source/input hashes, binary hashes and
complete journals are retained under the implementation checkout's
`target/completion-evidence/repr-membership/`. No Haskell worker or broad corpus
is needed for these repr-only fixtures; root owns integration checks.
