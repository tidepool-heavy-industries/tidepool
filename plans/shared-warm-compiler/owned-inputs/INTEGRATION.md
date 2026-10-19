# Owned row checkpoint integration packet

Status: implementation/source review and component checks accepted; baseline
reproduced both inherited fresh-worker oracle failures. Product fold remains held
until the approved second test-only correction is reviewed and its full case executes. No
product source is merged into the area yet.

Base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`.
Full candidate: `5113d9c962bdc06179628a0758cb0c768f3e5491` (second test-only
oracle correction; exact independent review/native gates pending), including accepted
row implementation `29d6467c309e53dbd0c1e9c89961c2eff24259c4`.
Branch: `feature/shared-warm/owned-rows`.
Checkout: `/srv/swarm/checkouts/tidepool-shared-warm-owned-rows-20261019`.
Area branch: `feature/shared-warm/owned-inputs` in the corresponding
`tidepool-shared-warm-owned-inputs-20261019` checkout. Preserve the complete
candidate ancestry when folding; no individual accepted code commit is orphaned.

## Change and review

Every ValueInterface is issued with dedicated directory/arena ownership by the
existing materializer, using the same factory as P6. A checked baseline borrows
its exact retained row/path instead of copying old interfaces into output
staging. Typed produced-output ownership, exact acquired facts, selected binding
and certificate guards remain distinct from physical sharing. New output
staging remains private. Packet fixtures retain their existing caller-owned
directory lifetime through a closed test-only custody variant.

The existing materialization cache distinguishes complete compiler contexts
from additive value leaves. Persistent current row maps share required
group/graph/scope owners without predecessor chains. Exact unchanged direct row
Arcs preserve the existing complete owner, avoiding an unnecessary fact detach.
No semantic namespace/home census/native choice, endpoint protocol, package
pinning, global registry or bound was changed.

Owned production files are `artifact_inventory.rs`, `checked_cell.rs`,
`declaration_context.rs` and `host_work.rs`; two nested test files migrate the
private materializer signature. The candidate also contains its producer
handoff. Full `base..candidate` whitespace check passes. Supervisor read the
critical production consumers and cumulative diff. Independent Sol-high exact
source receipts are `REVIEW-rows-49b65c8f.md` and `REVIEW-rows-29d6467c.md`.
The latter also verifies terminal native evidence without converting source
review into full joined qualification.

## Executed evidence

Private evidence root:
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/owned-inputs/rows/`.

| Exact source | Compilation | Counted execution | Evidence |
| --- | --- | --- | --- |
| `49b65c8f8220c0beb3e9b6b4ecdf5fc83654c779` | `just check tidepool-toolchain tidepool-runtime`, exit 0, production and test consumers | 18 selected/executed: 17 passed, 1 failed | `check-exact-provenance.log`, `native-results-49b65.json`, 18 copied case reports |
| `29d6467c309e53dbd0c1e9c89961c2eff24259c4` | Same check-plan labels through pinned Buck, `-j1 --local-only -c remote.enabled=false -c tidepool.profile=fast-dev`, exit 0, 27 local commands | 6 affected cases selected/executed/passed, each expected 1, exit 0 | `check-direct-reuse.log`, `check-targets-29d6467c.txt`, `native-results-29d6467c.json`, 6 copied case reports |

The supervisor reconciled both JSON summaries and all referenced report
presence: 24 executed records, 23 passes and the separately retained failure.
This does not combine different revisions into one exact-candidate suite pass.
The producer captured executable hashes before execution:

- Source49 toolchain libtest: `8811a18edd4b8c23b94752c8d7ebaea708b0c29f6c8bc04bd7ccf2792913512f`.
- Source49 runtime libtest: `f125f320d512c8f3ccfb5ebe8673196e892191efafcd83a4886cb38ead7ca562`.
- Source29 toolchain libtest: `776235e328c0de1ebbea9b459c7091e28a64e314146d96309e9419f450710ca0`.

Controls cover dedicated original/P6 row reuse, live V-only source ownership and
sibling release, sixteen changed-row overlays without predecessor growth,
unchanged direct-owner zero detaches, actual final FD/path release, concurrent
ordinary/prepared output capture, cancellation/retry, independent equal-payload
issuers, exact required dependency row reuse, packet ownership and both mandatory
optional-root/required-child regressions. The real checked-expression
publication/shadow/next-cell history passed at source49.

The failed source49 runtime case is
`session::turn::tests::paired_join_compiles_source_hidden_checked_bind_in_fresh_worker`.
It executed one case and failed at `turn.rs:8693` before its checked value/P6
path. It compares reserved logical `library()` identity to the lexical graph;
the actual `library_import_module()` may differ under certified publication
reuse. Host and independent source traces support a stale oracle, but
qualification owns the exact baseline execution discriminator. Preserve its
hidden-original, exact native-edge and subsequent real GHC/inspection controls
in any scoped test-only correction. Qualification subsequently reproduced the
same assertion at exact baseline `6d3b1745b465b6d0fe14104cb4020ead282f1620`,
selected/executed 1, failed 1, libtest exit 101 / runner exit 1. Its report is
`/srv/swarm/evidence/inanna/shared-warm-qualification-20261019/baseline-6d3/paired-join-case/8c6c6b90010c582d8e529fa11db53f1afac0ec1d8a11018970d3e83e32165b0b.json`,
SHA-256 `4fff5a9e049235a69dff22cb6bad587f853ad7d9b79e472d66ebac940039b124`.
Candidate a178 changes only this test's lexical-root oracle to the actual
`library_import_module()`, with explicit reserved-versus-actual distinction.
Supervisor and independent source review verified all later controls unchanged;
no production runtime code changed. The a178 runtime production/cfgtest build passed; its full case failed at the
next unchanged native-selection assertion (turn.rs8717), before target demand.
A controlled no-row baseline actual7c5c5d4c reproduced that same second failure
with only the lexical test patch over the6d3 native sources. Report SHA256
942f76b616f48fe0d94dfd15655bec8c42df2e79f0e66a21181b679c174fe4e3 and
exact input delta are in private native-oracle-baseline/TERMINAL.json. Campaign
approved a test-only pretarget-demand correction, preserving every downstream
actual target/native/refusal check and requiring both existing opaque-custody
owner controls plus the full corrected case. Rows owns that successor; independent
review and exact native gates remain pending. No graph census replaces opaque
offer authority. Failing original cache:
`/tmp/tidepool-libtest-cache-jv8erlca`. The area has not folded that test repair.

## Resource and continuation ownership

All row-owned clients from the row implementation gates are terminal. Direct status observation
found no active Buck commands. Retained daemon PID `3846846` and forkserver
`3846887` are the only processes observed in
`/build.slice/inanna-build-1792399526-3846769.scope`; that scope retained
3,389,693,952 bytes with historical peak 8,550,260,736 bytes at observation.
Daemon RSS was 866,384 KiB. The registered output mount remains in place;
one bounded scan measured 3,458,441,216 allocated scratch bytes. No output or
shared service was deleted/restarted. Daemon/output next use is the owned
test-only oracle repair and its exact checked-bind rerun after qualification's
baseline result. Keep them warm until that gate settles; retirement is separate.

Rows is active for the second corrected runtime case. Source_lifetime is active
for the root-authorized consumed-package-source proposal and then exact successor review. Both use the existing two reserved grants.
Use status-aware `followup_task` for their next concrete assignment. The area
supervisor owns the eventual full-candidate fold and campaign handoff. Campaign
owns the receiving branch and joined integration gate. Qualification owns the
baseline discriminator, actor/Persona bundle and measurement gates.

Remaining limitations are explicit: scoped cache/leaf enumeration and inherited
fact selection are not O(delta); dedicated rows increase FD/directory counts;
4096 transport bounds are unchanged and scale acceptance is pending. Package
authentication stays operation-scoped without a consumed-source lease. Required
native/prototype backing remains independently owned. Case reports do not
establish complete directory resource hashes or compiler END cleanup; actual
acquisition-absence, END uncertainty and joined actor behavior remain separate
qualification obligations. No speedup, release or deployment claim is made.
