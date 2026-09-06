# Strict-demand lane contract

Production owner: strict-demand implementation worker. Independent proof owner:
strict-demand regression worker. Coordinator reviews/integrates their candidates.

The existing distinction is load-bearing: `heap_force` follows thunk chains but
leaves closures alone. `is_lazy_poison` / `raise_lazy_poison` recognize deferred
bottoms; ordinary closures must remain valid WHNF and constructor fields must
remain lazy. Do not change every incidental heap_force call into deep demand.

A Case scrutinee is a strict WHNF demand even if it has only DEFAULT. A literal
case additionally demands a literal of its expected class before reading its
payload. Numeric unboxing demands the matching class through boxing wrappers.
A primop may have non-strict operands (inspect owning primitive contract before
changing shared argument forcing). An unused let or unselected constructor field
is not a demand. Carrying a deferred error must not itself raise it.

Implementation worker owns production code, source-level helper API/wiring, and
host unit tests. Prefer reuse/extension of existing strict-demand mechanisms;
new helper signatures must have production callers, not a parallel registry.
Regression worker owns additions to existing integration test files only, using
existing registered suites: no manifests or shared suite registration edits.
Report baseline JIT success vs reference error as proof; never encode incorrect
behavior as a passing expectation. Pin literal/default/data case demands,
boxed/unboxed demands, dead branches, lazy constructor fields, ordinary function
WHNF and original error identity. Keep fixture Haskell adjacent when needed.

The worker scopes are independent and both may recurse. Reviewed integration
must run the same exact regressions against the merged repair revision.
