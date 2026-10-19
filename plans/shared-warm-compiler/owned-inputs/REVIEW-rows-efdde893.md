# Independent original-value factory review

Task: `/root/warm_compiler_campaign/owned_inputs/source_lifetime`, rows Wave 1.
Base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`.
Candidate: `efdde8930756507015616e3cb48fdee5593b2416`.
Checkout: `/srv/swarm/checkouts/tidepool-shared-warm-owned-rows-20261019`.

Verdict: one exact-acquisition repair remains before source acceptance. Earlier
promotion association, same-view omission, predecessor-chain and value sibling
backing findings are addressed at their production owners. Native compilation
was running under the producer's admitted resource owner during review; this
review did not poll, modify or claim its outcome. The producer separately
reported three stale cfg(test) call sites for its new materialization argument
and owns their repair. No reviewer build/test/resource action was performed.

## Remaining finding

`declaration_context.rs:4126`, `issued_value_row`, accepts a `Private` row when
the stored entry is pointer-identical **or structurally equal** to the current
entry. `ArtifactEntry::eq` delegates to `ArtifactFactsHandle::eq`, which first
checks its private `same_owner` but then explicitly falls back to structural
Original/Canonical/Joined equality. Independently acquired equal-byte joined
facts can therefore borrow another issuer's physical row through this path.
That contradicts the new constructor's exact-acquisition claim and the
equal-payload independent-issuer contract.

Expose a closed owner-local `ArtifactEntry` boolean using
`facts.same_owner`, with metadata consistency and the existing current selected
binding/acquisition checks preserved. Do not use the existing
`same_issuing_authority`: its source/catalog witness comparison returns true for
two ordinary joined entries with no such witnesses, so it is too broad here.
Add a same-ArtifactId/equal-payload but independent-facts refusal control and a
positive exact-owner borrow. Source packet reuse remains governed by its typed
actual output issuer and independent selected-context validation; it does not
need a digest registry or reissued logical support.

The earlier recommendation that the existing facts owner distinguishes
independent acquisitions refers to the closed `same_owner` method. Structural
`Eq` does not preserve that distinction; this review verified that difference
directly and sent the correction to rows and the area supervisor.

## Production path and lifetime observations

The original materializer now sends every ValueInterface entry through
`RetainedArtifactRow::issue_value`, sharing its existing mutable package
validator. Both generic arena appends and generic directory writes exclude those
entries. The factory derives paths, parts, seals, lengths and requirements from
the admitted entry. Thus live immutable fixed frontiers may retain an old value
row, but that row owns only its value's interface/package backing and cannot pin
another value sibling. Required generic native/canonical dependencies remain
separate and may retain broader backing legitimately.

P6 capture queries an existing dedicated row after selected binding/acquisition
checks and distinguishes borrowed versus fresh factory work. Packet-local typed
issuance still serializes promotion and separates independent equal-byte output
packets. The original-materializer-first regression now keeps V-only N and its
requests live while dropping C/S; it checks one original V descriptor survives,
S's directory disappears, P6 uses the original V path, and fresh packet promotion
count stays zero. This directly addresses the independent immutable-fixed
retainer rather than hiding it by dropping N.

The complete cache overlay uses persistent rows, no predecessor materialization,
shared group/graph/scope owners, and returns the same Arc on an unchanged overlay.
The repeated-promotion control constructs sixteen original packet issuers,
retains the old in-flight request, and checks current ownership shape `(1,16)`
through replacements before dropping old descriptor owners. The code avoids the
previous triangular BTreeMaps and wrapper chain. First overlay still traverses
the physical closure; subsequent lookup scans current scoped cache references,
which the handoff reports rather than claiming constant metadata work.

Private and test-only Packet value custody are distinct. Durable fixture child
directories remain beneath the caller-owned packet and disable their own
cleanup; the packet owner removes them after consumption. `issued_value_row`
refuses Packet custody, so a durable packet path cannot masquerade as an
independently retained checked row. Failure cleanup remains with the existing
packet-root rollback. Normal runtime Private rows retain independent TempDirs
and sealed file owners. Generic request FD enumeration and END settlement still
use the existing endpoint owner and unchanged wire format.

Diagnostics now distinguish expected captured bytes and selected sealed
transport metadata from observation of the mutable alias file. The direct test
mutates that alias, preserves the sealed endpoint body, and checks the diagnostic
manifest retains both observations. No mutable alias rehash becomes authority for
an owned baseline.

## Unqualified boundaries and next action

The source tests cover the intended authority and lifetime routes, but this
review has no counted execution evidence. Producer-owned native compilation,
focused tests, required optional-root/required-child regressions, actual runtime
BindingIndex refusal, missing-acquisition overrides, real checked worker/actor
use and END uncertainty remain separately observed gates. Dedicated rows raise
FD/directory counts; the existing 4096 arena/image transport limits and broader
FD scale acceptance remain unchanged and unqualified.

Rows and area received the exact remaining acquisition finding. Next producer
action: owner-identity comparison plus discriminating control, stale test-caller
repair, terminal native-check evidence and exact successor OID. Then request the
reserved reviewer's cumulative re-review. Do not fold this candidate as accepted
solely from source or formatting checks.
