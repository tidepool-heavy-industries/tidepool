# Exact successor source review and joined lexical failure

Reviewer: `/root/warm_compiler_campaign/owned_inputs/source_lifetime`.
Producer: `/root/warm_compiler_campaign/owned_inputs/rows`.
Base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`.
Prior cumulative source receipt: `49b65c8f8220c0beb3e9b6b4ecdf5fc83654c779`.
Candidate: `29d6467c309e53dbd0c1e9c89961c2eff24259c4`.
Implementation checkout: `/srv/swarm/checkouts/tidepool-shared-warm-owned-rows-20261019`.

## Successor disposition

Source-only acceptance of the bounded successor atop the cumulative49 receipt.
The production delta removes the kind guard only when every direct leaf row is
already the same Arc in the complete owner's direct map. Direct rows override
inherited rows, so this establishes that no selected physical fact changes.
Inherited-only rows still take the existing initial detachment. Structural
equality, IDs or seals cannot substitute for Arc identity.

The new `direct_complete_promotion_reuses_owner_without_fact_detach` control
first prepares the original direct generic value row, then enters actual P6
capture. It asserts zero fresh promotion, identical interface path and the same
complete Arc through four retained/prepare rounds. Its thread-local counter
increments at the actual initial `select_custody_facts` branch and remains
unchanged. The test would fail under the preceding kind guard. Existing changed
row history controls remain necessary negative coverage; an equal-byte new row
must still replace the current physical frontier while preserving old in-flight
requests. The empty-leaf branch is unchanged.

The trace reports actual direct lookup visits (including short-circuiting) and
zero detach on the shortcut. Scoped cache scanning and leaf matching remain;
this is neither constant-time acceptance nor a measured speedup. `git diff
--check` passed; the reviewer ran no build or tests. Successor compilation and
counted execution remain producer-owned and pending at this receipt.

## Exact49 joined failure

Inspected `native-results-49b65.json` and `test-runtime-fresh-worker.log` in
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/owned-inputs/rows/`.
They retain exact source, pre-execution artifacts, counts and outcomes:16
toolchain cases and1 runtime checked-expression history passed,1 runtime fresh
worker case executed and failed. The failure is `session/turn.rs:8693`, before
hidden-import inspections and checked-bind/input-work assertions. Cache retained
at `/tmp/tidepool-libtest-cache-jv8erlca`. This is not joined acceptance.

The failing assertion requires `context.lexical_graph()` to contain
`view.library().module_name()`. The owning contract distinguishes:

- `SessionCompileView::library()` returns the reserved logical/native declaration
  generation through `CompileLibrary::original`.
- `library_import_module()` returns the actual compiler interface. A Certified
  library uses `projection.module_name()`; a recovered library uses its evidence
  root. `turn_imports` consumes that import identity.
- `lexical_projection::issue_projection` issues a
  `Tidepool.Actor.Surface.H<digest>` module, then creates the context lexical root
  from that exact receipt, plus admitted surface lexical dependencies.
- `paired_publication::reusable_projection` checks current admission, final
  generation, exact exports, instances, families, surface and original products.
  `PublicationEvidence::ReusedProjection` deliberately couples the new logical
  publication generation to the unchanged compiler receipt. `stage_materialized`
  clones that projection's context. `matches_generation` checks the logical
  reservation separately rather than requiring a renamed receipt.

Thus the existing assertion is incompatible with the accepted reused-projection
route. Its current form was introduced in `033980ec182`; the library identity
split is present since `8a8e3ee9874`/`cfa54554d97`, and the reusable-projection
gate since `058cbcea977`, all ancestors of the assigned base. The exact same
assertion and split are present at base5b74. Host_inputs independently confirmed
this owner chain. No production lexical repair is justified by this failure.

## Row-route discrimination

Before the assertion this test opens an empty session, validates and adopts a
declaration, retracts a type head, publishes declarations, retires the private
scope and removes original source/interface aliases. It creates no runtime
value binding or checked bind. Declaration term-type queries in
`validate_rendered_module` are strict inspection requests, not Val interface
issuance. The declaration fixture's functions and constructor belong to its Lib
original/canonical products. The new P6 `capture_output` and checked-value leaf
promotion routes are not entered by this prefix.

Declaration projection preparation does materialize interfaces, through
`declaration_join_inputs` -> `materialize_interface_inputs`. This is the existing
interface-only owner, separate from `materialize_retained_artifacts` and its
dedicated ValueInterface factory. Exact compilation for earlier probes may
prepare generic native/canonical/lexical facts, but there are no issued Val
entries or leaves in this setup for the changed value branch or overlay to use.
The row cache stores physical observations; `lexical_graph` reads immutable
`owner.facts.lexical`, issued by the paths above. The cumulative row diff neither
rewrites that lexical field nor changes runtime projection selection. This
source chain establishes a stale test expectation; it does not claim an executed
base comparison or prove the later checked-bind workflow passes.

## Minimal next observation

If execution attribution remains required, the smallest controlled comparison is
the unchanged exact test at base5b74 in an isolated admitted checkout with the
same pinned compiler/profile/resources and a fresh per-run cache. Retain the
executable hash and executed1 outcome. Do not rewind the producer's active
checkout or reuse its failure cache as a cold control. Equal failure at8693
confirms the preexisting assertion dynamically; different behavior requires
capturing the actual library identity, actual import identity, receipt root and
lexical owners on both versions before diagnosing a row regression.

The owning runtime test repair should assert the issued actual projection root
via `library_import_module`, retain the logical generation check separately,
and keep the hidden-original negative lexical/import controls and exact native
group retention checks. That discriminates genuine projected visibility from a
weakened assertion. The reviewer made no runtime edit and started no build.
The owner must then rerun the entire nonzero fresh-worker case through the
checked-bind/input-work assertions; fixing this early oracle is not a workflow
pass. Rows owns successor checks; host/area own lexical test repair and joined
qualification.
