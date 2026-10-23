# Runtime/publication group

Lead: /root/type_consumer_resume, implementing P13/P14 in successive coherent
parcels. Pending candidate307592 is safety-oriented but its extra retained-input
ledger must be folded into the existing resolver. Keep actual compiler transaction
settlement as the preparation-to-runtime boundary.

## P13 scoped resolved inputs — group lead

PersistentSession issues one scoped resolved owner with full compiler occurrence,
actual value ID/defining scope/PreparedHandle/checked row/source custody/lease.
Derive native import/commitment indexes, keep existing persistent suffix sharing.
Delete separate SOURCE prebind/native ledger/installer re-resolution owners.
Checked-row immutable compatibility is checked once without arena materialization.
Dynamic re-entry still checks scope reachability, revocation, epoch and live handle.

Acceptance: old23/new91 INLINE+NOINLINE, hidden exact bindings, foreign equal-payload
scope refusal, missing runtime value after cold compiler recovery, no repeated
compiler closure traversal or per-value reachable-scope reconstruction.

## P14 execution cursor and captures — group lead

Replace reservation/in_flight Options with closed CellPhase.
Affine reserved/ready/running tickets own exact item/frontier/generation/capture
plan. Completed result carries CapturedCheckedValues, whose installation updates
BindingTable, BindingIndex and provenance together, then advances committed prefix.
Preserve Parked/Failed outcomes and prior effects; PriorStage becomes live only
after actual producer success. Keep mutable checks on exclusive session re-entry.
Delete duplicate compiler-prefix append/check and reconstruction by binding name.
Remove production finite native-setup/host-carrier branch used only by tests,
migrating those tests through actual World/host issuer. Preserve legitimate
ActivationPreview/HostPrototype and actual host capability checks.

Acceptance: cancellation/suspension/resume exactly once, stale/foreign ticket
refusal, failed producer cannot admit consumer, capture ordering and actual
checked-row/handle pairing, independent final lease/root release.

## P15 frozen publication intent — /root/runtime_publication_owner

Own paired_publication and narrowly coordinated Resident freeze callsites.
Only completed private frontier issues FrozenWorldExecution with mandatory World
evidence/private lease. Closed binding/declaration payload, no optional parallel
records permitting partial success. Retry reuses exact private result and effects,
refreshing only public CAS baseline. Remove raw-ID freeze test-only production
entrypoints after fixture migration. Cold recovered compiler context is never
LiveCheckedValue/RunningItem/completed private execution.

Acceptance: publication contention/retry does not repeat effects or select newer
historical values; incomplete/refused execution cannot freeze; missing/revoked
private/source leases refuse; real declaration/binding/no-change success.

## P16 typed failures — /root/transition_failure_owner

Own leaf failure vocabulary and conversion boundaries, coordinate edits with
subsystem owners rather than rewriting their active blocks. Each transition
preserves original typed cause and stage/owner coordinates. Dynamic runtime
reasons include OwnerChanged, ScopeRevoked, WrongPhase, SourceRevoked,
RetainedValueUnavailable, PreparedValueChanged and PublicBaselineChanged.
Parse/shape, certified facts, permission and mutable lifetime failures stay
distinct. No universal mega-error/state-machine framework and no string routing.
Migrate broad stale and provenance bool adapters as their owners land.
Normal tracing is bounded/optional; error observations don't issue authority.

Acceptance: exact causes survive conversion/recovery; errors do not become false
absence/success; no loss of primary failure through cleanup/persistence wrappers;
normal logs can be filtered without changing assertions or semantics.
