# Unified main integration

Status: accepted for unified main. This record supersedes the historical
applications and sleep launch maps; it does not authorize a new swarm launch.

## Source and scope

The Tidepool merge combines main `892c5c511a33402d44e752299e5ac4b9cd7cf2ff`
with accepted foreground work `76a95f1e802cdeea4b491d473000063ecb603c83`.
Native Codex main `d0e5fd48e08db62f164d9087a12dbcd87d659a6a` combines its
flat-tool implementation with accepted applications source
`db2442eccc5da8561b3d922dccfa1a67654beb6e`. The final native executable passed
the actual CLI/TUI fixture and all four focused ACK/presentation tests.

This integrates resident sleep, exact native-session input, hosted completion,
retirement and source recovery into the current command/resource/tool owners.
The prepared-engine rewrite and shared-server migration remain separate work.
Engine-only tests depending on `MachineDisposition` were omitted; the current
runtime's source-reconstruction test remains and passes.

The original development checkout is preserved on
`archive/rsi-build-snapshots-20260911` at `026c3833`. Its superseded source is
not an additional integration branch. Untracked runtime `.shoal` state remains
untouched.

## Reconciled ownership

- **Native session and input:** the native embedded app server owns execution
  and the durable input ledger. Its exact dispatch claim records presentation
  and consumes the matching queue row in one transaction. Acknowledgment
  compacts confirmed terminal evidence; it cannot prove delivery. Transport
  reattachment preserves the same binding and rejects another thread.
- **Hosted evaluation and completion:** native persistence gates context forks;
  the resident actor retains the exact workbench call journal. Recovering a
  failed root transfers replay evidence into a fresh authorized successor.
  Identical settled retries return the retained receipt; unsettled calls stay
  fenced, and changed input cannot reuse the call identity.
- **Cancellation and sleep:** flat `haskell` calls use the same cancellation
  owner as resident evaluation. Cancellation registers its wake before observing
  state. Sleep suspends the effect program; delivered steering or notifications
  interrupt it without replaying earlier effects or executing the remaining
  suffix. This is live-session behavior, not persistence of suspended programs.
- **Resources and retirement:** the existing deployment owner retains producer
  sealing even when native binding fails. Native process scopes, accepted hosted
  work, command jobs, sockets and workspace/build resources retain their current
  owners. Unconfirmed cleanup remains retained custody.
- **Tools and environment:** flat shell tools and Haskell commands share the
  Commands owner and resource limits. A failed hosted tool does not enable a
  second native shell launcher. Workspace forks retain their Bubblewrap and
  copy-on-write ownership. Shared-server work must preserve these boundaries.

## Executed acceptance

Tests below used local scripted providers, with no paid model inference. Native
checks use the native repository's pinned Rust toolchain; Tidepool checks use
the Nix environment and its immutable extractor. Running acceptance binaries
were frozen at distinct paths so concurrent compilation could not invalidate
their `current_exe()` paths.

| Boundary | Executed evidence |
|---|---|
| Native TUI, completion, cancellation, reconnect, command output | 60 focused native TUI tests passed on `889e4ca` |
| Native actual CLI/TUI input | `full_tui_attaches_host_owner_and_routes_correlated_input` passed |
| Acknowledgment does not manufacture delivery | Three native ACK tests passed on `d0e5fd48e0`, including Ready/Dispatching/Unknown rejection and lost-response idempotence |
| Matched Tidepool/native input | `pinned_full_tui_binds_and_accepts_exactly_one_owned_input` and provenance rejection passed; presentation asserted before ACK |
| Root replay and fresh authority | Dropped-reply recovery, forest recovery, unsettled-call fencing and exact-call retry tests passed |
| Root reentry policy | `abnormal_root_reuses_only_a_queue_ready_conversation` passed |
| Registry incarnation and state | `resident_reentry_state_tracks_unavailable_busy_and_stale_checkout` passed |
| Retirement and binding failure | 15 hosted-retirement tests passed, including retaining the exact producer seal after bind failure |
| Real sleep | Actual TUI fifteen-minute test passed in 939.28 seconds, with no intermediate inference |
| Sleep interruptions and console output | Four actual-TUI tests passed for delivered interruptions and ordered console output |
| Actor scheduling | Controlled-clock fifteen-minute sleep and sequential record-actor handler tests passed |
| Command resource isolation | Hosted actual-TUI OOM/steering fixture passed all 44 scripted requests in 680.80 seconds; native-shell OOM fixture passed in 31.76 seconds |
| Recursive workspace forks | Final-package actual-TUI fixture passed in 125.65 seconds: source isolation, inherited untracked files/mtimes, fresh Cargo artifacts, busy-workspace fallback, OOM and subsequent steering |
| Source reconstruction | Current-runtime declaration recovery integration test passed |
| Input transport and owner seams | Focused tracked steering, restart/query, lost acknowledgment, native push and notification-barrier tests passed |
| Generated Haskell corpus | `just fixtures-check`: 217 passed; fixture payloads unchanged |
| Shared package | Workspace package check compiled the shipped Project/Shoal modules; live API-guide example passed |

Final-pin native acceptance passed 1/1 actual CLI/TUI and 4/4 state tests.
Tidepool's matched input fixture passed 2/2 on the final immutable package,
including provenance verification and confirmed presentation before ACK.
Final-pin fixture regeneration changed only the source fingerprint;
`just fixtures-check` passed 217/217 again. Rust formatting and diff checks passed.
The final-package recursive-workspace fixture and both cheap fixture preflights
passed. No required acceptance check remains open.

All changed Rust build and test targets were compiled. The codegen test suite's
only change is module ordering; its target was compiled with `--no-run`, not
executed as a broad suite. The workspace fixture now derives resource setup
from production defaults and has ordinary tests for configuration compatibility
and success/failure receipt recognition.

The matched package is recorded at
`target/unified-main-ack-20260911/staging/selection.json`. Its native executable
is `/nix/store/60j5k8qpgc5badz0jfjc5i2nsgvf894f-codex/bin/codex`, SHA-256
`c3e54d111b240deae9dd42a0fd6da34f88465db7bdf377cc721a33205b29ec8e`.
Shoal SHA-256 is
`c5d292d026f8312a3c96c9ec7b504da0008ff002e020d9483414f0614108b304`.
The resource and long-sleep tests used native `889e4ca`; its only subsequent
production change is the terminal-only input ACK repair, covered by the
final-pin state and actual-TUI input tests. No running binary was replaced.

## Release limits and next work

No full workspace battery or native aarch64 acceptance is claimed. Recovery after
Shoal loss starts from Git/history; live handles and suspended Haskell programs
are not reconstructed from source. Retained historical swarm resources need
their own preservation/cleanup audit and are not evidence of a live deployment.

A future swarm starts from the final unified main and a frozen matched package,
with newly commissioned work. Do not recommission the historical applications
or sleep lanes. The later shared-server migration must separate native session
identity from pane/process identity while retaining the input, completion and
execution-environment owners above.
