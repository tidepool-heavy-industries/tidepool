# Wave 3: schema closure and tagged nursery

Baseline: `204604dc19ce526dde3d717f3c8e0078714a5002`.
This is a review checkpoint, not production cutover. Retained regions, old
space, broader lowering and resident integration remain deferred.

## Shared contract and execution

The lead owns semantic decisions and review; Luna High owns implementation.
All six briefs below were written before spawning. Start A, B, D; admit C at
the first vacancy, then E after D, then F after all implementations finish.
Three workers maximum. Shared worktree: exclusive file ownership, no worker
commits, no concurrent builds. Ask the lead for the build slot before Cargo,
Cabal, regeneration or tests; release with exact command/results. Use the
repository dev shell. Read root and nearest AGENTS.md plus
scripts/codex-worktree-guidance.md. Use apply_patch. Do not format other owners'
files. Report changed files, commands, counts, failures and remaining checks.
After two unsuccessful attempts on Luna, escalate to Terra with findings.

Review clarification: global declaration metadata is checked before RHS walks.
Publishing all top types may therefore report a later top's invalid constructor
before an earlier RHS scope error. Source-order diagnostics apply within the
subsequent walks, not across declaration/body phases. Pin this deliberately in
a regression rather than preserving an accidental list-order dependency.

### Tag and collector invariant

Managed-reference policy belongs in tidepool-heap::managed_reference. Tag 0
means no evaluatedness evidence; 1..6 mean the corresponding authoritative
constructor tag; 7 means evaluated with descriptor inspection. Canonical tags
for constructors numbered >=7 and functions/PAPs are 7. This deliberately
differs from GHC's small-family and function-arity convention. Constructor
descriptors carry a nonzero authoritative tag. Thunk/evaluating/continuation
objects have no evaluated tag. Tagged nulls and contradictory tags reject.
Raw Address/scalar bits are never masked or retagged.

Centralize tag_of, untag, tag_valid. Allocation returns an untagged address;
the existing Construct emitter initializes the object then ORs its constant
descriptor tag before publishing the managed result. That is the only new
generated tagging point. ABI v2 becomes v3 in both languages; schema stays v4.

The complete pre-copy linear walk proves pinned descriptor identity, extent,
header state and exact object starts. Initial Forwarded headers reject.
Exclusive collection may then dereference those proven identities directly.
Mask managed references before range/start checks and preserve valid tags.
Updated chains are chased iteratively with reusable visited bits/path scratch
reserved before mutation. Cycles reject. A short-circuited slot receives the
relocated indirectee with the update's tag bits, validated against its final
descriptor, not the original thunk's zero tag. Forward every traversed thunk
to this result, accounting for different thunk/result extents. An Updated
chain must end at an evaluated constructor/function/PAP, not null, continuation
or an unevaluated thunk. Integrity failure remains terminal; both buffers and
descriptor/code owners survive native unwind. Preserve first cause, semispace
reuse, exact-live growth, and no host call on generated allocation fast paths.

## A — validator (Luna High)

Own only tidepool-repr/src/execution_schema/validation.rs, including its tests.
Implement three settled fixes together because they overlap this owner. Make
binding_type return Result and obtain constructor reps through constructor(),
propagating errors through top publication and local recursive-group actions.
Publish every top value before walking any RHS. For malformed wire only, hide
a NonRecursive top's own slot during its RHS using one undo entry; restore it
afterward. GHC emits self-reference in Rec groups, so add no new scoping
feature. Preserve recursive visibility, closure epochs, local non-recursive
scope, program-wide uniqueness, both validation passes and source-order errors.
Replace hand-calculated check_layout rules with comparison to
StorageLayout::for_reps, retaining rep checks and canonical root-mask checking.
Add regressions for invalid constructor IDs in recursive top/local groups,
reversed cross-module top dependencies, non-recursive self-reference rejection,
and noncanonical padded layout rejection. Preserve existing deep/scope tests.
Do not edit schema constants (D owns them) or generated fixtures (F owns them).
Request the build slot; run bash scripts/dev-shell.sh cargo test -p tidepool-repr
--lib execution_schema:: and compile the execution_schema_codec target if
needed for a changed public contract. Format only your file and run git diff
--check. Report exact counts, changed-file list, and any premise needing lead
resolution; no commits, broad battery, or unrelated repair.

## B — projection (Luna High)

Own haskell/src/Tidepool/ExecutionIR.hs, ExecutionProjection.hs, and Haskell
projection tests/adjacent source fixtures. Read haskell/AGENTS.md. Repair target
closure using the existing ExecutionIR binding traversal: gather references
against the complete supplied top-binder universe, reuse traversal rather than
add a competing STG walker, and expose only the minimal helper consumed by
projection. pmBindings annotations contain imported free variables, not home
edges; do not use them as the target reachability graph. Preserve exact
unit/module/occurrence identities, recursive-group atomicity, GHC emission
order, and explicit package imports. Include function occurrences, constructor
fields, nested bodies and argument references already covered by the walker.
Add a producer regression targeting polymorphicIdentityResult that retains
polymorphicIdentity and demandedCallee transitively while excluding unrelated
tops. Retain full-program and binder-uniqueness regressions. Do not edit
ExecutionSchema.hs or encoder/ABI tests (D owns these); do not regenerate CBOR
or fingerprints (F owns regeneration). Request the build slot, then run
bash scripts/dev-shell.sh bash -lc 'cd haskell && cabal test execution-schema-projection'.
Run git diff --check and applicable existing Haskell formatting checks without
rewriting unrelated files. Report the exact command, selected/executed counts,
dependency identity mechanism and changed files. Ask the lead about unsettled
premises; do not approximate dependencies or suppress failing regressions.

## C — retirement (Luna High)

Own tidepool-macro/, examples/guess/, examples/tide/, root Cargo.toml and
Cargo.lock, tidepool/Cargo.toml, tidepool/src/lib.rs, and live references to the
retired macro/examples in build configuration or user documentation. Read
tidepool/AGENTS.md before its files. Remove the obsolete macro crate and both
macro-driven Core examples completely, including workspace members/dependencies
and facade export. Do not restore InlineInput, haskell_eval!, the interpreter,
or invent replacement frontend support. Preserve other examples and all
production Shoal/session behavior. Search all tracked callers and build/package
configuration for references; remove only now-invalid live references, not
historical checkpoint evidence. Report any overlapping file before editing it;
plans/stg-production-cutover.md and friction_notes.md belong to F. Regenerate
Cargo.lock through Cargo, never hand-merge it. Request the build slot before
Cargo metadata/lock generation or checks. Run bash scripts/dev-shell.sh cargo
check -p tidepool --lib, plus metadata validation confirming removed members
are absent. Workspace --tests compilation belongs to F after all parcels;
do not run it independently. Use apply_patch for tracked deletions, format
changed Rust only, and run git diff --check. Report every removed directory,
remaining live references, exact check outcomes and unrelated blockers without
repairing them. No commits; git history preserves the removed examples.

## D — tag ABI (Luna High)

Own tidepool-heap/src/managed_reference.rs (new), lib.rs and
execution_descriptor.rs, required mechanical descriptor-construction migrations
in heap/codegen tests, codegen prepared_native.rs/descriptor_bridge.rs, Rust
execution_schema.rs ABI constant and Haskell ExecutionSchema.hs ABI constant.
You may own codec/encoder ABI rejection tests, but not validation.rs or Haskell
projection files. Follow the shared tag contract exactly. Make constructor
tags immutable nonzero descriptor metadata, centralize tag_of/untag/tag_valid,
and use the owning constructor API to prevent untagged metadata inventions.
Migrate all descriptor constructors, preserving layouts. Existing Construct
initializes raw storage and publishes a tagged result; host consumers untag
before dereference. Do not broaden supported expressions. Bump ABI to 3 in
both languages, retain schema 4, and add explicit ABI-2 rejection coverage.
Test all tag classes, invalid evidence/tagged null and untouched Address bits.
Do not regenerate fixtures; F does this after integration. Request build slot
for focused heap descriptor/tag tests and codegen prepared/descriptor tests
through bash scripts/dev-shell.sh. Stale fixture ABI failures are expected
until F; distinguish those from compile failures. E will later own collector
behavior and related tests; limit raw.rs edits to necessary constructor-call
migration and hand ownership off explicitly. Format changed files, diff-check,
report exact tests/counts and all affected consumers, no commits or broad runs.

## E — collector (Luna High)

Start after D's handback. Own tidepool-heap/src/gc/raw.rs and collector tests;
execution_descriptor.rs only for necessary typed collector errors after D
releases it; codegen host_fns/gc.rs for focused contract tests. Implement the
shared collector contract without changing root lifetime or failure disposition.
Keep the complete linear header/start walk, then eliminate redundant descriptor
HashMap lookups in evacuation and scan using the proven pinned identity.
Validate incoming tag evidence, untag before range/start tests, and preserve
tags on rewritten roots/fields. Chase Updated chains iteratively with reusable
visited/path scratch allocated before mutation; detect cycles explicitly and
forward every traversed thunk to the relocated tagged indirectee. Do not test
forwarded targets against the eliminated thunk's descriptor or extent. Preserve
late-error terminal semantics and both-buffer ownership, deduplicated roots,
exact-live growth and ordinary semispace reuse. Add tests for tagged roots and
fields, unchanged raw addresses, long/shared chains, corrupt cycles, differing
extents and repeated references. Request build slot and run bash
scripts/dev-shell.sh cargo test -p tidepool-heap --lib, then codegen --lib
host_fns::gc::tests::prepared_ through the same shell. Report exact outcomes,
unsafe proof obligations and any sub-item needing correction. No old-space
owner, emitter expansion, unrelated fixes or commits. After two failed Luna
attempts the lead escalates this parcel to Terra, naming the failing sub-item.

## F — fold and evidence (Luna High)

Start after A through E finish. Own generated fixtures/fingerprints,
docs/stg-projection-inventory.md, plans/stg-production-cutover.md,
friction_notes.md and the trial record below. Integrate the shared tree as-is;
there is no rebase step. Use exact source baseline above and record the final
revision. Regenerate neutral prepared fixture using its documented Haskell
probe and canonical fixtures only through repository commands. Never hand-edit
CBOR, fingerprints or lockfiles. Acquire the exclusive build slot for this
entire fold. Run owning prepared native/runtime/toolchain integration tests
against regenerated fixtures, workspace compile-only through dev-shell, then
just changed 204604dc19ce526dde3d717f3c8e0078714a5002 and just fixtures-check
serially. Record exact commands/counts; unrelated reds are not your repair task.
Update inventory for actual dependencies, ABI3/tag conventions and deferred
old space. Correct macro blame (introduced by e1a4b914, not unchanged from
main) and CBOR float history (pre-existing by source inspection, not a baseline
run); do not conflate separate float tests. Ask lead before any semantic repair.
Run formatting checks/diff-check. Prepare corrected draft-PR text and handoff;
commit/push only when lead authorizes the reviewed file set. Record worker and
lead rounds, outcome path and disagreements per task. Token usage unavailable
unless harness evidence exposes it; add no other measurement framework.

## Trial record

Final F evidence is recorded at working-tree revision
`204604dc19ce526dde3d717f3c8e0078714a5002` (dirty before the authorized WIP
commit). The fold regenerated the neutral fixture and canonical corpus through
repository commands; `just fixtures-check` passed. E's repaired collector
reported 49 heap tests plus 3 prepared-GC tests, A reported 49 schema tests,
and F's regenerated prepared-native, runtime `session::prepared`, and
toolchain `prepared_artifact::` selections passed 4/4, 2/2, and 3/3.

G's fresh-producer reruns remain red: 11 generic-deriving tests and one
repro-339 test failed (12 total), with prepared-representation, duplicate
top-level `sat`, and Word(32)/Word(64) signature failures. The workspace
compile-only gate reached all crates but is blocked by undefined
`sibling_server` in `tidepool-actor/tests/resident_local_actor.rs`; the
changed-file inner loop stopped before tests on formatting diffs. These are
pending/unrelated blockers, not a green-workspace claim. Dropped differential
scenarios and preserved explicit JIT assertions are documented in the fold
evidence; no replacement coverage or performance baseline is claimed.

Counts distinguish assignment/review/correction rounds; tool polls are not new
worker attempts. Rounds are A 4, B 3, C 2 plus Terra 1, D 1, E 3 plus Terra
repair, and G 3. Lead assignment/review/correction totals and worker/planner
token usage are unavailable from collaboration handbacks; brief-size
insufficiency is recorded, and E's sub-item escalation is resolved. See
`plans/stg-production-cutover.md` and `plans/next/shoal-commit-forks.md` for
the integrated handoff and Shoal planning context.
