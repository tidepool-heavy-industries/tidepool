# Warm worker implementation handoff

Area owner: `/root/warm_compiler_campaign/warm_workers`, Sol high.
Checkout: `/srv/swarm/checkouts/tidepool-shared-warm-warm-workers-20261019`.
Branch: `feature/shared-warm/warm-workers`. Initial source/contract:
`5b74c2f1fe010c5c343e0c6f331d54328fbd5090`. C2 amendment
`e2bd2611102566085d0c573568d820a50ed62fb3` is incorporated as
`6beec6fd58c5ae27ee47ff86fa5f2797895adb9c`; its STATUS bytes were taken
unchanged from that amendment. Only the campaign owner edits shared contracts/status.

## Live ownership

| Native task | Configured model/effort | Checkout/branch | Start/settlement | Work |
| --- | --- | --- | --- | --- |
| `/root/warm_compiler_campaign/warm_workers/protocol` | `gpt-6.1-sol`, high, fresh context | `tidepool-shared-warm-worker-protocol-20261019`, `feature/shared-warm/worker-protocol` | Running; start before 2026-10-19T07:54:42Z; no settlement observed | Protocol implementation at `992a6d0458`; now bounded review of the area's independently authored WorkerServer response route |

The initial grant is one active descendant total. No other descendants were
started. The area directly owns the existing WorkerServer typed completion path,
its response fixtures and the compiler retention inspection. The campaign owner
can independently review this work without another area child grant.

## Reviewable source checkpoints

`b62961652eda5d24535b36de33406885e3feda54` removes the unused
`completedOriginalEnvironment` and redundant `completedModuleEnvironment`.
The latter's only consumer repeated the same final-HPT/owner lookup stored in
`completedModuleHome` at the sole constructor. A missing HMI therefore still
reaches the existing finalized-home fallback. Lazy HMI/recovery owners remain;
this deletion does not establish complete release or a measured memory gain.
`2ea510f075ccc8b7a7f5620faac884e610666b61` corrects package-source wording:
producer identity binds a libdir path, while package reuse requires a retained
consumed-source owner.

The campaign supervisor independently reviewed the full
`5b74c2f1..2ea510f075` source delta, including the imported amendment and
missing-HMI path, with no remaining findings. This is source review only.
Production/test compilation and executed controls are deferred; this is not an
accepted realization wave or qualified area fold.

Private protocol candidates `1e84fd4323`, `1ca6b9697b`, `992a6d0458` are
incorporated as `fd0cd43237`, `fcb39fd9bd`, `55a26d8d85`; area `6469ed1b85`
owns the joined typed completion/response tests. `c402df90fe` applies the pinned
formatter. These are source checkpoints pending cumulative independent review
and native compilation. The source-free production dispatch currently returns
typed UnsupportedOperation. It issues no Ready/reference and is not a completed
realization consumer.

## Current decisions and dependencies

- The host-input owner extends `CompilerSourceSupport`; its selected Arc is
  an authenticated process-local borrow, not portable authority. Its portable
  root and authenticated delta/base seam remain dependencies. Do not derive
  root identity from payload digest or broader custody.
- The private protocol groundwork uses the existing versioned envelope and
  physical input descriptors. Complete acquisition refers to the existing
  portable exact-scope manifest. There is no acquisition-reference registry.
  Checked delta is omitted until the real authenticated base exists.
- `WorkerServer` currently renders diagnostic JSON on its machine channel and
  forwards captured stdout to its human channel. The pending typed completion
  change carries source-free outcomes as typed body results; printing a Ready
  result would not implement the protocol. Capture/cleanup failure must override
  a typed successful outcome and preserve existing END ownership.
- Package freshness needs actual consumed-byte ownership or a fresh compiler
  universe before user source. Current deployment hashes frontend/worker and
  libdir text, while lazy HMI details retain shared EPS/finder cells. Fresh
  path hashes cannot bless an already loaded older EPS. Owned-inputs/source_lifetime
  traced the same counterexample. A retained qualified Nix closure at NEW boot
  may enable reuse only after DB/config/interface/native selection coverage;
  otherwise decoded cross-operation reuse remains unavailable. The existing
  multi-stage operation must keep one compiler universe through completion.
- Existing completed source/original maps, universe HPT/recovery, request input
  decoder, lazy HMI knots and interpreter-loaded objects are independent strong
  owners. A new root cache's entry count alone cannot bound their residency.
  Keep existing memory admission and RSS/request rotation; no budget CLI or
  swap experiment has been added.

## Evidence and pending commands

The area buck-out bind mount is provisioned and observed with `findmnt`.
No Buck daemon, project build, compiler worker or live host was started here.
Observed `/srv/build` available bytes at 2026-10-19T07:54:42Z:
`108974125056`, about 1.49 GiB above the required 100 GiB reserve.
There is no defensible cold-build growth bound within that headroom. The
installed launcher does not support the staged storage-status/growth options;
no admission bypass, output deletion or shared-service restart is authorized.

Executed: `swarm-build just check-plan tidepool-extract-cmd`, exit 0, metadata
selection only. `swarm-build bash scripts/buck2-configure.sh --tests` completed
exit 0 with rooted generation `generation.dVqJhqs0`; log:
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/warm-workers/configure-tests.log`.
The exact ce60 workspace submodule was initialized from the verified local
repository with a one-command file-protocol override. `swarm-build bash
scripts/dev-shell.sh rustfmt --edition 2021` on request/endpoint/fake-worker
sources passed, and `git diff --check` passed. These do not establish native
compilation or executed tests. Exact pending commands, after admissible storage:

```sh
swarm-build just build //bridge/haskell:tidepool_extract_bin //bridge/haskell:worker_response_child //bridge/haskell:worker_response_test_bin //bridge/haskell:cell_splitter_test_bin //bridge/haskell:source_boot_product_reuse_test_bin
swarm-build just check tidepool-extract-cmd
swarm-build just test-native //bridge/haskell:worker_response_test --pattern '/response bounds async unwind and next request/'
swarm-build just test-native //bridge/haskell:worker_response_test --pattern '/source-free cleanup failure suppresses Ready/'
swarm-build just test-native //bridge/haskell:source_boot_product_reuse_test --pattern '/completed compiler request histories/'
swarm-build just fixtures-check
```

Discover exact native cases and Rust case names before recording execution
counts. The source-free request/typed-response candidate remains uncompiled
and unqualified until both matched owners and their real test consumers run.
Qualification owns the joined packaged Persona/cell/lookup history; prior exact
70a Persona evidence is 1 passed/1 executed, exit 0, with host cleanup confirmed,
compiler cleanup not observed and incomplete compiler diagnostics.

## Continuation

The latest user direction forbids periodic wake/poll loops. Complete ready
review/repair/fold work, then record exact dependencies and yield. A producer
whose result unblocks this area checks native task status: use send_message for
a running recipient and followup_task for an idle/completed recipient. Pending
producers are host-inputs for the exact portable selected root, owned-inputs
for admitted row/byte owner and consumed-package-source coverage, qualification
for a measured focused compile growth bound, and the campaign supervisor for
the cumulative independent review. There are no orphaned command observers.
