# Sequential bound adapter handoff

The Tidepool facade retains `LocalResidentInstallation.policy` when
`PolicyInstalled` reaches the host. That is the actor's
`ResidentInteractivePolicy::local_with_installation` endpoint, backed by its
single `InstalledToolsState`; a reconstructed endpoint does not carry that
state. Each model request calls the endpoint's fallible
`snapshot_for_request`. Its immutable manifest, handler and source lease stay
together through later calls and cancellation. Raw custom input stays raw;
function input stays structured JSON. Unsupported snapshots fail before model
transport, with no live-endpoint fallback.

The harness companion is
`c485edb9b697ffc671b22c9ef25a73fc84763d76` on
`integration/actor-admission-companion`, based on the operation-identity
candidate. Tidepool `Cargo.toml` and `Cargo.lock` name that exact commit at the
public Git origin. Both companion and adapter candidates are pushed; the
local adapter check used an environment-only Git URL redirect to the clean
companion worktree and has not verified a remote dependency fetch. Add the
exact Git source hash to both `cargoLock`
users in `flake.nix` before a Nix distribution build.

The facade's `EmbeddedHarnessRuntime` opens one Store and one configured
`JobScheduler` under the existing run directory. `EmbeddedHostActor` holds the
exact actor, installation and a durable-envelope wake sender. A counted
`ActorAdmissionLease` protects only synchronous Store admission; the actor
endpoint fences tool enqueue separately and owns execution after acceptance.
The run ID comes from the existing `runtime_namespace(run_root)` owner.
The composition owner must supply the actor path from its descriptor or owner
registry; the adapter invents no child path. Attach rejects another run or
actor incarnation before Store binding. Retirement delegates to actor
shutdown. Interrupt remains unsupported pending exact operation semantics.

The harness uses its Store as the mailbox authority. `Conversation::input`
commits before it sends an envelope-ID hint and drops the admission guard
before waiting on wake. The Engine rereads the Store, so duplicate hints and
reconnection do not duplicate content or claim delivery without a request.
`OperationId` carries the embedded run, actor, incarnation, issuing request
and original call ID through dispatch and cancellation; no job-handle lookup
registry or bare-call-ID fallback is introduced. The bound provider exposes
only the harness's mailbox wait verb plus the host's pinned tool manifest.

Reload has an intentional mixed outcome: source publication can succeed while
spec compilation or surface comparison fails, leaving the previous handler
active. A request snapshot captures the actual installed source and handler
at issue time; an earlier snapshot keeps its prior lease. The adapter does
not treat source and handler publication as one atomic reload.

## Evidence and remaining gate

- Harness `embedded_host` target: five exact tests passed, covering pinned
  request dispatch, parked durable input with duplicate hints, reconnect,
  cancellation while parked and unavailable-snapshot failure before model
  transport. The same target compiled four other tests without executing them.
- Tidepool `tidepool` library target compiled with the exact companion commit;
  its real resident actor bound to an offline embedded Engine parked, received
  one durable input and resumed. The exact nextest selection passed 1/1
  (548 skipped), with the per-run compiler daemon torn down.
- The default Codex backend was not switched. Remote dependency resolution,
  Nix Git source hashes, bound actor-path composition from the owner registry, and
  complete M1 raw/structured/retained/cancellation/reload/reconnect/Codex
  integration gates remain for review. Concurrent execution remains disabled.

## Cancellation and retirement repair checkpoint

The companion adds a typed `CancellationAcknowledgment::Completed` result.
The owning `JobScheduler` publishes it through its existing first-settlement
lock, so an actor `Expired` reply can retain success even when the provider
waiter has not resumed. One focused companion regression passed 1/1: a late
provider result did not overwrite the owner-observed success. The adapter
maps `Expired`'s typed workbench reply to that acknowledgment and takes the
existing short actor admission lease while capturing a request snapshot.
Its retirement race test passed 1/1 through the GHC-aware battery (549
skipped): a published installation remained available internally, while a
new request was rejected after admission closed. A direct Cargo attempt
compiled the target but could not execute because `TIDEPOOL_EXTRACT` was
unset; the battery supplied it and passed. Broader integration is unverified.
