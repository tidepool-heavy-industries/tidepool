# Full-TUI process supervision and custody

Status: planned. Implements slices A4 and A5. The terminal and recovery decisions
are part of option A, not a migration to a headless app-server.

## Result

Each tmux pane launches a small process supervisor, which launches the normal
Codex TUI on that pane's real terminal. The supervisor retains the exact
`ServiceScope`, direct child and namespace-init witness for the lifetime of that
native application. Shoal can lose its coordination connection without losing
the process owner or the person's interactive terminal.

Ordinary successful retirement obtains real process and hosted-work completion,
then settles the exact resource leases. Unknown cleanup retains those leases and
reports which evidence is missing. Pane disappearance never substitutes for
process completion.

## Owners and implementation shape

Extend `tidepool-node/src/process_scope.rs` and `process_boundary.rs`. Add the
supervisor loop and its typed local control protocol in small modules in
`tidepool-node`. Expose a hidden internal entry point through the existing parser
in `tidepool/src/bin/shoal.rs`, composed through `tidepool/src/shoal.rs`; do not
build another command parser or standalone installed orchestration product.

Dispatch the hidden helper before initializing the full Shoal host runtime,
compiler or providers. Use a small current-thread I/O loop and at most one scope
worker for blocking process operations. Keep control/status responsive while that
worker waits, with a bounded command queue and retained result slot. Do not create
a multi-thread host worker pool in every idle pane.

The existing `actor_host` deployment row retains a noncloneable scope client
anchored to its exact launch reservation and installed `ActiveBinding`. Adapt
`actor_host/scoped_custody.rs` to that actual production consumer. The staged
direct-spawn adapter is not a second supported launch architecture. The underlying
direct-child scope remains the implementation used by the supervisor.

The helper has no actor registry, inbox, provider client, Haskell interpreter,
model prompt, continuation policy or Codex input authority. Its operations are
limited to preparing, releasing, observing and stopping its one OS scope. It
cannot create a second payload or adopt a different actor after failure.

Use the existing private deployment directory for one immutable launch manifest,
the scope socket and a small versioned checkpoint. Reuse durable atomic writes
and migrations. This is per-launch evidence held by the existing deployment
owner, not a second durable process registry. Do not include secrets in status,
trace output or the tmux command line; pass private manifest paths and preserve
existing credential/environment handling.

## Preserve the process-scope proof

Carry forward the detailed
[namespace scope contract](../next/service/namespace-scope-contract.md). The
existing implementation is not merely a process-group kill wrapper.

- Use the audited bubblewrap default new PID namespace with its own init.
  Do not substitute `--as-pid-1`, a reused PID namespace or process-name matching.
- Acquire the init identity while the payload is blocked. Retain the proc
  directory, open its pidfd and validate the fresh identity/namespace observations
  according to the current scope owner.
- Preserve the blocking launch gate and the init-held duplicate writer passed
  through `--sync-fd`. Closing the host-side gate must not accidentally release
  the payload through EOF.
- Retain the direct bubblewrap monitor child and wait it separately. Under the
  checked platform contract, exact namespace-init exit proves namespace drain;
  direct monitor reaping is an additional obligation.
- Never signal a serialized or rediscovered bare PID. Cancellation before identity
  can be pinned may leave a blocked namespace and must report that uncertainty.
- Dropping an uncertain scope may attempt identity-safe emergency termination,
  but cannot produce a successful cleanup receipt.

The source audit referenced bubblewrap 0.11.0 and Linux ordering examined at
v6.12.63. A matching version string is not runtime conformance. Add a real platform
fixture that exercises gate retention, init identity and descendant drain with
the packaged bubblewrap. Record the checked executable/kernel identity in
acceptance evidence. Unsupported scope prerequisites fail automated launch before
the payload is released. Do not silently return to legacy pane-based cleanup.

The namespace covers the native application and its local descendant processes.
It does not cover Haskell execution in the Shoal host, remote processes started
through another daemon, or persistent effects owned by another interpreter.
Those require their existing owners' completion evidence.

## Real terminal contract

Replace the current `spawn(environment, output: File)` assumption with a typed
stdio policy supporting the existing captured-service case and an inherited
interactive terminal case. The supervisor inherits tmux's file descriptors 0, 1
and 2 and supplies the same terminal to the native payload. There is no nested
PTY broker, keystroke forwarding, terminal emulation or screen scraping.

The process owner must establish correct foreground process-group and controlling
terminal behavior while retaining the namespace gate. Keep the supervisor out of
the payload's foreground input path; use the existing POSIX process primitives to
place the payload and restore the terminal when it finishes. Handle terminal
handoff and `SIGTTOU` deliberately. The helper does not read stdin or write status
messages over the TUI screen. Its diagnostics go to a bounded private log.

Preserve window resize, Unicode, paste, alternate screen, interactive approvals,
native tool output and interrupt behavior. Do not forward a signal both through
the terminal and through the control channel. A terminal suspend or foreground
handoff must not be mistaken for process exit. Restore saved terminal settings
after abnormal native termination where the controlling terminal is still valid.

This is a release gate, not a future polish task. Real PTY tests must prove normal
TUI input and interactive child commands through the scoped launch. If namespace
or job-control behavior prevents that, fix the owning launch/stdio mechanism
before enabling the new production path. Do not satisfy the gate with a read-only
observer or a pipe-based smoke test.

## Launch transaction

Every step has one retained owner before it starts an asynchronous operation.

| Step | Durable/retained fact | Failure rule |
|---|---|---|
| Reserve actor deployment | Existing lifecycle row, exact actor and launch identity | No native work yet; settle only resources definitely created by this reservation |
| Install checkout and resource leases | Existing worktree/build owners and frozen selection | Publish no actor capability before the actual binding is installed |
| Prepare private endpoints and manifest | Exclusive directory owned by the row | Collision never authorizes deletion of an existing path |
| Start hosted endpoint | Accepted hosted work has its retained service handle | Endpoint startup failure follows existing hosted retirement |
| Submit tmux helper launch | Row records that a helper may exist before submission | Lost tmux result is uncertain; do not resubmit another helper |
| Pair helper with row | Fresh challenge plus exact launch identity and private endpoint | A helper cannot release its payload for a different reservation |
| Spawn blocked native wrapper | Helper immediately retains direct child, gate and witness resources | Store spawn result before notifying the host; lost notification does not erase custody |
| Pin namespace init | Helper owns exact init witness | Failure retains uncertainty or proves cleanup through that same owner |
| Release payload | Host row and hosted endpoint ready; helper applies release once | Lost release acknowledgment is queried, never repeated as a new launch |
| Bind native session | Exact live binding from A1 | No automated bootstrap/input until all readiness obligations succeed |
| Deliver bootstrap | Durable A3 envelope | Failure follows the normal delivery state machine |

The native bridge cannot register before the payload is released. Therefore the
release precondition is the retained process/host infrastructure and verified
installation, not a circular requirement for an already running native session.
Native binding is the subsequent automated-input gate. If native negotiation
fails, report launch failure and preserve the TUI for manual inspection according
to the existing failure policy.

Before release, loss of the host connection must not launch the payload. The
helper attempts bounded cleanup once it has an exact init witness. If identity is
unconfirmed, retain the blocked state and report it. After release, host
disconnection preserves the payload and helper. These are distinct phases, not a
single unconditional kill-on-disconnect policy.

Render the helper invocation as structured arguments through the existing tmux
launcher. Keep shell escaping with that owner. The helper's manifest contains the
already selected native command, environment and `ProcessMountBoundary`; it does
not rerun prompt selection, path discovery or worktree allocation.

## Scope control protocol

Use a versioned private local protocol with exact launch identity on every
request. Its first version supports:

| Command | Contract |
|---|---|
| Pair / inspect | Challenge the helper, return its immutable launch identity and current phase |
| Prepare | Spawn at most one blocked wrapper from its immutable manifest |
| Pin | Acquire the existing scope witness or return retained uncertainty |
| Release | Release that pinned scope once; repeated identical release observes its state |
| Stop | Terminate and wait the exact scope; repeated stop resumes observation of the same scope |
| Finalize | Close control admission after terminal evidence has been consumed; never spawn again |

Long blocking scope operations run outside actor and terminal event loops. Client
timeouts stop waiting, not ownership. The helper retains the operation and its
result so a subsequent inspect/stop can obtain the same evidence. A host task
must not own the last reference to an unsettled deployment row.

The scope checkpoint records launch identity, phase, whether release may have
occurred and diagnostic terminal observations. It is useful for crash inspection,
but a JSON receipt cannot create a release capability. Ordinary settlement flows
through the row's original, paired scope client. Recovery can reacquire limited
authority only by the fresh live-helper procedure in
[05-recovery.md](05-recovery.md).

If the helper crashes, no replacement helper may adopt its serialized PIDs or
claim its missing monitor wait. Preserve the retained resource obligations. A
missing helper socket is not a successful native stop, even if the pane looks
dead. This conservative exceptional failure is acceptable; the normal path must
nevertheless settle successfully.

## Retirement transaction

Use the existing actor terminal state, retained fleet and hosted retirement
objects. Refactor the transaction into small modules with one production caller;
do not grow another lifecycle map alongside them.

1. Record the actor terminal decision through its existing owner. Close new
   assignments, updates, notifications, hosted call admission and pending child
   launch admission for this deployment. Start native producer sealing. Retain
   all resulting futures/results before awaiting them.
2. Quiesce the hosted endpoint while allowing completion acknowledgments and
   necessary status queries. Preserve the existing distinction between quiescing
   and fully draining HTTP. Abort unpublished forks through the fork owner when
   retirement makes their admission invalid.
3. Request interruption of the observed exact native turn when available. Give
   accepted work its bounded existing graceful retirement opportunity. A changed
   turn or lost native connection does not prevent authorized scope termination.
4. Request native scope termination and cancellation of accepted resident/effect
   work through their respective owners. These operations can progress together;
   neither may wait indefinitely for an acknowledgment from the component it is
   preventing from finishing. Record a terminal aborted hosted call when the
   native recipient is gone; never synthesize a persisted result or release a fork.
5. Obtain exact namespace drain and direct monitor wait from the retained helper.
   Obtain hosted/resident/external-work completion from the existing runtime and
   hosted retirement owners. Then drain and join the hosted HTTP service.
6. Let the existing custody owner combine those obligations with its terminal
   actor and exact installed binding. Only that owner can settle the worktree,
   socket and build-resource leases. Do not accept an externally supplied copy of
   a sibling's process receipt or a `cleanup_confirmed` boolean.
7. Finalize the helper after its terminal evidence is consumed. It closes
   admission and all checkpoint/log writes before confirming final drain. Remove
   its private directory only when the directory owner has that evidence and all
   other endpoint work has drained. A lost finalization response retains the
   uncertain directory; it need not re-fence a separately settled worktree.
8. Apply the requested pane policy. A retained completed pane is a UI artifact,
   not an active process lease. Remove/close a pane only after process accounting;
   do not kill it first and then try to infer cleanup.

Replace `retire_native_pane` as the process authority. Replace permanently failing
post-submission release branches in build/socket/worktree custody with exact
owner-driven transitions. Preserve legacy submitted actors as unconfirmed; new
evidence cannot retroactively make their old launch scoped.

Releasing an active binding is not deleting a worktree or discarding its commits.
Keep the registry's existing preservation policy. Build-resource settlement
releases this launch's lease; shared cache deletion remains with its owner and
must account for other users.

## Native exit and failure retention

Watch the helper's exact scope state throughout the deployment, including after
binding. Native exit triggers actor failure/terminal handling even when the
hosted endpoint remains responsive. Tmux observations remain useful diagnostics.

If hosted coordination fails while native execution continues, mark coordination
disabled and retain the application for manual use. Keep the helper and leases
anchored by the retained fleet. Do not drop the scope on that policy transition.
An explicit later stop uses the same owner and can still produce successful
cleanup. A deadline returns structured outstanding obligations and leaves that
continuation addressable.

## Acceptance

- [ ] Real PTY launch preserves typing, paste, resizing, Unicode, approvals, interruption and native tool output.
- [ ] Interactive child commands receive the expected terminal and foreground behavior.
- [ ] Host loss before release cannot execute a marker payload, including when the host gate closes.
- [ ] Host loss after release leaves the original full TUI usable and the helper retaining its exact scope.
- [ ] Lost tmux, prepare, release and stop replies cannot create a second payload or lose custody.
- [ ] Killing the native application with descendants leaves cleanup pending until exact namespace drain and monitor wait both complete.
- [ ] A missing/dead pane with a surviving descendant cannot settle resources.
- [ ] Helper crash or unpinnable identity retains explicit uncertainty without signaling a reused PID.
- [ ] A normal completed actor and an explicitly cancelled actor both reach successful production lease settlement.
- [ ] Process cleanup alone cannot release a lease while an accepted hosted Haskell call or external effect is live.
- [ ] A sibling's copied receipt, stale launch identity or forged scope status cannot settle another actor's binding.
- [ ] Retirement timeout leaves the same owner available for a later successful stop.
- [ ] Socket collisions, partial launch and finalization-response loss preserve other actors' resources.
- [ ] Failed coordination preserves the TUI, while intentional retirement eventually stops and accounts for it.

Use isolated test panes and processes. Do not demonstrate these cases by killing
the developer's active Shoal sessions.
