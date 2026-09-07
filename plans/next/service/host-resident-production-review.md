# Host resident consumer: partial review checkpoint

Reviewed production candidate `934eac12fdcc99e881da446e58a3478b0c30b2fa`
and test-only repair `6e8167f5b82a0ba9ccdb0a3f553a4b8956c807a2`.
**Not approved for complete integration acceptance.** Native launch, native
cleanup, final custody settlement and live-host replacement remain gated.

## Accepted ownership and test repair

The existing `InteractiveOwners` row anchors the hosted slot before launch.
`hosted_retirement::start` installs control, original gated service task and owner
before opening the gate. Workers do not capture a map/owner back-reference.
Seal/shutdown futures remain in `Operation::Pending` while a waiter borrows them;
cancellation cannot discard the original operation. Finished results remain
addressable through `recover_hosted`. A failed/foreign seal cannot later turn
into terminal-path success. Completion remains usable until explicit Abort.
HTTP drain and exact resident cleanup are separate from native cleanup.

The earlier assertion that no public pending-shutdown test seam existed was
incorrect. The repair uses public `spawn_local_actor` and `KernelBehavior` with
a real shutdown-hook gate and genuine actor-issued seal. Dropping the waiter
and a bounded timeout preserve the same pending shutdown; hook count stays one.
Releasing the gate yields Confirmed hook, Unsupported realm and HTTP still
pending, not fabricated positive cleanup. Actor/service tasks are explicitly
joined; the owner weak reference expires. Separate authored-actor coverage
establishes the positive confirmed-cleanup path.

Independent focused Nix validation:
- At original candidate: live authored positive plus pending foreign seal,
  2 passed / 150 skipped, 19.969s; run
  `732da506-05c4-4f77-9309-64afcdb24c8f`.
- At repaired candidate: new pending shutdown plus live authored positive,
  2 passed / 151 skipped, 16.518s; run
  `7a8b929a-378f-4324-9b7f-ef0661f917e8`.
- Changed library test target compiled. Formatting and diff checks passed.
- Repaired test binary SHA256:
  `5d6d2d9456d04be871406bb0082bd5640fdb47e2ef0335338f7410e41fb6239b`.
- Extractor SHA256 (verified, correcting earlier transcription):
  `463d2664aea5b9e776efacd1ed7d1659735998caf340c17c676cb375401e2c93`.

Reviewer logs are under `target/hosted-production-review/`. Implementer build
and manifest evidence is under its retained checkout
`target/custody-evidence/hosted/review120-{build.log,manifest.txt}`.
Shoal compile is attributed, not reviewer execution or a live-host test.

## Blocking actor-owner capability checkpoint

The already-terminal path bypasses endpoint sealing and only quiesces its HTTP
control. `start(slot, actor, server, listener)` accepts actor and service
independently. `LocalResidentInstallation` also exposes independent public
actor/policy fields. No owning invariant pairs the endpoint with the actor whose
retained cleanup authorizes HTTP drain.

A retained regression patch pairs an already-terminal real authored actor with
another still-live campaign's real policy endpoint. It observes TerminalPath,
Accounted confirmed cleanup for the expected actor, and Drained HTTP while the
endpoint actor remains live. Both campaigns and the service are cleaned before
the failing assertion. This is a fixture-level reachable mismatch, not a claim
that the current normal launch producer actually supplies a mismatched pair.
The negative regression is evidence, not committed failing test source.

Concrete minimal handoff: actor owner should expose the existing canonical
`ResidentInteractivePolicy::local(LocalActorRef)` Rust constructor (currently
`pub(crate)` in `tidepool-actor/src/resident_interactive.rs`) or supply an opaque
actor/canonical-policy pair. Host `start` can then accept the exact actor plus
HTTP config/listener and create its own canonical service. Arbitrary endpoint
fixtures must remain untrusted and cannot use the terminal cleanup fast-path.
Do not manufacture a seal from stopped-actor failure, use a public bool/label as
identity, add a registry or duplicate the resident dispatcher. This requires
peer-owned actor API authority, outside this review's edit scope. Retained
implementer remains available for repair once the capability is delivered.

Reviewer independently reran the negative regression at `6e8167f5` plus
`initial-terminal-gap.patch` (SHA256
`c75642c899e096591b89942058ad0cb565131920baa49fbbde5e91d5c8544f1f`).
Expected failure reproduced: 1 failed / 152 skipped, 20.201s, run
`f60ab257-29a5-45f4-bfc7-8f6945b33882`; log `known-gap-tests.log`, hashes
`known-gap-hashes.txt`. The patch was reversed and source equality to the
candidate checked afterward. The negative binary is not the passing candidate
binary and remains the last local test binary; no binary was installed/launched.
