# B — Keep package custody distinct from HOME ownership

Own the existing acquired-owner index in
tidepool/toolchain/src/artifact_inventory.rs and the HOME-specific consumers in
certified_products.rs. Coordinate shared certification edits with A.

## Established mechanism

ViewLease::insert_payload indexes every payload by ExactModuleIdentity, including
PackageCode. SelectedOriginalGroupScope::has_home_owner delegates to the generic
observed-owner query. A package can therefore make its own owner look like a HOME
module and trigger the guard against downgrading a HOME reference to a package.

The guard is meaningful; its input lost the distinction. Do not remove the guard
or globally narrow has_observed_owner: other consumers intentionally ask about
generic observation. The captured-A failure occurs during generation-zero cold
recovery, before its intended fencing assertion. The exact offending module has
not been observed, so its causal attribution remains to be demonstrated.

## Approved shape

Enrich the value of the existing owner index with a small typed count distinction
between HOME payloads and PackageCode. Keep one owner and constant-time queries.
Use an exhaustive match on the actual payload/ArtifactKind: Original, Canonical
and joined Interface roles are HOME; PackageCode is package custody. Do not infer
kind from module spelling, labels, paths, pointer addresses or native readiness.

Expose separate generic-observed and HOME-observed queries. Migrate only the
HOME contract, including its View, Acquired and Contribution variants. Derive
counts from actual retained payloads once; querying HOME must not scan a whole
graph or manufacture permissions.

Maintain both facts through parent union, insertion, replacement, pruning,
restoration and removal. The existing replacement early return for equal owner
names is insufficient if the category differs. Deduplicate by the inventory's
actual payload identity, not by the number of aliases/bindings retaining it.
This index is derived state; recovery can reconstruct it without a new format.

## Acceptance and prevention

- Compare the maintained index against an independent census of actual payload
  kinds over generated insert/replace/merge/remove histories. Use shrinking and
  observable coverage, not the same update routine as the oracle.
- Package-only owner: generic observed true, HOME false.
- Genuine HOME plus package under one owner: both observations true; removing
  package keeps HOME; removing the last HOME keeps generic observation but clears
  HOME. Include identical references and overlapping distinct snapshots.
- Exercise the actual package/HOME classifier with a production-issued positive
  control and a genuine HOME downgrade refusal; custody still grants no execution.
- Re-run captured-A recovery through the real owner, preserving later stale-A
  refusal and fresh-B success. A setup failure is not cutoff acceptance.

Diagnostic-only candidate 543d737e63307a1ba59f32c14c588fa218dfcb1e in
/srv/swarm/checkouts/tidepool-captured-a-owner-diagnostic-20261021 records the
consumer/import and both predicate results without weakening the guard. It is
unjoined and unexecuted. Reuse its bounded evidence if attribution still needs it;
do not blindly merge it as the repair or keep a permanent verbose debug layer.

The prevention is a kind-aware owning query plus its census property, not a new
comment telling callers to remember what the generic count includes.
