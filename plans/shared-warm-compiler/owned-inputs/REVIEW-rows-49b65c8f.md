# Independent cumulative checked-row source acceptance

Reviewer task: `/root/warm_compiler_campaign/owned_inputs/source_lifetime`.
Producer task: `/root/warm_compiler_campaign/owned_inputs/rows`.
Base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`.
Candidate: `49b65c8f8220c0beb3e9b6b4ecdf5fc83654c779`.
Checkout: `/srv/swarm/checkouts/tidepool-shared-warm-owned-rows-20261019`.
Branch: `feature/shared-warm/owned-rows`.

Verdict: **source-only acceptance** of the cumulative assigned scope. No
remaining confirmed source finding after direct review of the production
producer-to-consumer path and repairs. This is not compilation, executed-test,
performance, integration-readiness, release or deployment acceptance.

The reviewer performed no build/test/resource action. The producer's native
check at `efdde893` failed exit 3 on three stale cfg(test) callers of the changed
materializer signature. Candidate `49b65c8f` explicitly migrates those callers
to `OwnedArena`; its native recheck and all focused execution were still pending
at the source verdict. The area subsequently reported the successor native check
running in session `14002`, expressly admitted by the campaign with a 12 GiB
allowance against the prior 7.18 GiB scope peak. Rows owns terminal observation
and selected tests next; source is frozen. No compile/count result is inferred
from that running state.

## Accepted ownership path

Every newly materialized ValueInterface uses one private
`RetainedArtifactRow::issue_value` factory from an admitted `ArtifactEntry` and
the existing operation-local package validator. Values are excluded from both
generic shared-arena appends and shared-directory writes. Each Private value
row owns only its interface/package directory and sealed arena, plus its exact
issuing immutable facts. An immutable fixed frontier can therefore retain that
row without retaining another value sibling's bytes.

P6 capture validates selected support, promotes or borrows the row before the
existing support projection, then retains that row with exact certificate,
receipt, source/template/native support and prototype. A typed
`CheckedValueRowIssuer` couples the actual produced certificate and private
cancellable slot; ordinary/prepared captures cannot pair an arbitrary populated
slot with another value. Lock ordering is view cache → packet slot, with no
reverse acquisition in the factory. Fallible construction completes before
slot publication; failed attempts leave no cached row. Separate packet facts
and current support admission remain independent of physical byte sharing.

Private existing-row reuse requires current selected binding, actual acquired
payload and `ArtifactEntry::same_issued_facts`. That closed boolean uses
`facts.same_owner` plus descriptor and requirements; structural `Eq` is not
authority. The new control deliberately supplies equal ArtifactId and structural
facts from an independent owner and verifies refusal, a new path for fresh
issuance, matching seals, positive fresh work and valid exact-owner reuse.

Checked baselines preserve duplicate/missing/excess certificate and requested
byte mismatch refusals, borrow the issued path/row, and use shared byte-owner
identity only after matching the selected certificate. Separately supplied
buffers still compare. Import authority retains the complete checked artifact;
its certified path uses sealed transport instead of rereading mutable aliases.
Diagnostics separately retain expected bytes, selected transport metadata and
observed alias-file bytes/errors.

Complete and leaf cache entries remain typed. Leaf-only sources preserve fixed
and ancestor visibility, with leaves following ancestor facts. Same-view cold
materialization includes own leaves, and complete hits use a current persistent
row frontier. Overlays retain row/group/graph handles and shared scope bytes
directly with no predecessor materialization. Old in-flight requests keep their
own old owner; current cache revisions do not retain a growing chain of maps.
Hidden retained custody with no physical row is not promoted into a materialized
input. Exact compiler metadata, namespace, native choices and home census are
unchanged.

Private runtime and test-only Packet custody are closed variants. Fixture rows
live beneath the caller's packet cleanup owner and cannot be borrowed as
independently retained checked rows. Runtime rows retain their own TempDir and
sealed descriptor. Existing request file enumeration and compiler END settlement
remain with their original owners and unchanged wire format.

Strong checked-artifact equality preserves original path, exact row identity,
producer/receipt, certificate, compiler support, lexical/template evidence and
prototype. The actual runtime BindingIndex still consumes that equality; the
patch does not reduce it to bytes or digest equality.

## Regression coverage reviewed in source

The controls now distinguish the known failure categories:

- Same produced packet through concurrent ordinary/prepared capture, shared
  authority and one physical promotion; wrong selected context and cancelled
  capture leave promotion empty before valid retry; deterministic cancellation
  while an owner lock is held.
- Original materializer V+large S creates two separate descriptors; V-only N and
  its requests remain live while S's directory/descriptor owner releases; P6
  borrows original V with zero fresh promotion work.
- Same-view cold and preexisting-complete routes preserve the actual leaf path;
  sixteen replacements retain one current owner with sixteen row references,
  preserve the old in-flight request and release its old descriptors afterward.
- V requiring preexisting A preserves A's original row/path and the value leaf
  across sixteen selected/native/template histories.
- Independent equal-byte output issuers remain distinct; requested-byte mismatch
  and duplicate certificate selection refuse; independent equal-payload acquired
  facts cannot borrow an old row.
- Final borrower destruction observes row weak references, actual directory
  paths and `/proc` descriptor endpoints. Mutated diagnostic aliases leave the
  selected sealed body intact and visible in diagnostics.
- Durable fixture cleanup remains packet-owned, and Packet rows cannot issue
  independent checked custody.

These source controls are not reported as passing tests. The admitted command
list and producer evidence paths are in `HANDOFF-rows.md`; compilation and
nonzero execution at this exact candidate remain mandatory.

## Accepted limits and continuation

Required native/canonical rows, issued native offers, catalog source owners and
original host-binding prototype contexts can retain their actual necessary
backing. This review accepts no claim that all such backing becomes value-sized
or that worker/GHC caches are collectible. Dedicated value rows increase
directory/descriptor counts. Existing 4096 arena/image format bounds remain
unchanged; scale/FD acceptance is pending.

The implementation traces actual factory work and cache/row/FD enumeration.
Same-view hot lookup still scans scoped cache references; neither constant
metadata work nor a runtime speedup is established. Package source ownership,
fresh observation mode and stale EPS/native-source limits remain as recorded in
`PACKAGE-SOURCE-FINDINGS.md`; this row parcel does not issue package pinning.

Next action: producer obtains the agreed native lane, reruns affected production
and test compilation, executes focused counted ownership/refusal controls and
mandatory optional-root/required-child regressions, and reports exact source,
artifact, counts, status and logs. The joined owner then validates real checked
worker/actor use, missing-acquisition overrides, runtime BindingIndex refusal and
END uncertainty at the folded candidate. A source-only review receipt does not
waive those gates. Rows and area supervisor received this exact verdict.

## Bounded unchanged-overlay cost follow-up

While exact `49b65c8f` counted actions remain frozen, reviewed the parent's
proposed fast-path change without modifying implementation. It is sound to
return the existing complete owner whenever every proposed leaf is already the
same exact row Arc in `complete.rows`, regardless of `MaterializationKind`.
Direct rows override inherited rows in the existing traversal. Thus this test
establishes no custody or graph/scope/native change and needs no closure detach.
Keep actual Arc identity; payload equality or ArtifactId alone is insufficient.
An inherited-only row absent from the direct map still takes its initial detach.

Discriminating control: create a direct original materialization of a value,
retain its complete Arc before P6, perform actual P6 borrowing with zero fresh
factory work, then repeatedly use same-view retained/prepare paths and assert
the complete Arc is unchanged and no physical-fact detach is observed. Preserve
the no-leaf unchanged case. The existing V-only N control has an inherited V and
does not establish this direct-row shortcut. Equal bytes with a genuinely
different row Arc must still update the physical frontier and preserve the old
in-flight owner. Trace or owner counters should separate cache/leaf matching
from actual closure detachment.

This removes a needless common detach but retains scoped cache-key/leaf scans
and direct-row matching work. No constant-time claim or measured speedup follows.
Rows and area received the bounded verdict; source changes wait for current
actions to settle, then need an exact successor review and affected checks.
