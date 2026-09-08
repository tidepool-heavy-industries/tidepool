# Native workspace admission checkpoint

Codex branch `work/build-snapshot-admission`, commit `5e914a95ee`, in
`/tmp/tidepool-build-snapshot-codex`, based on pinned `d760c5cb8c`.
The Tidepool runner pin and launch environment are unchanged. This is not yet
an enabled or complete native snapshot protocol.

## Implemented boundary

`codex-utils-pty::workspace_admission` owns a shared mutation gate and an exclusively
created Linux writer cgroup. Executor commands enter that group before exec in
both pipe and PTY paths. A task-local scope includes actual executor commands;
ordinary infrastructure process launches are unchanged. `codex-sandboxing` applies
the scope at its common local spawn entry point. The shared patch runtime holds
a mutation guard, including shell-intercepted patches. Hook commands and shell
snapshot initialization use the same owner's direct-command spawn entry point;
their existing output, waiting, and cancellation owners remain in place.
The older `core::spawn` executor, asynchronous Git commands, and configured
PowerShell version probes also use that entry point. Captured commands share
Tokio-compatible output handling in the admission owner.

Snapshot admission requires the exclusive gate and kernel `populated = 0` on an
opened cgroup events file. It does not infer completion from shell exit, process
group identity, output EOF, or elapsed silence. Detached descendants remain
accounted for. No builds or TUIs are cancelled. Private opt-in environment metadata
is `CODEX_WORKSPACE_SNAPSHOTS`; unavailable cgroup custody disables snapshot
admission while preserving normal native execution.

## Checked

- This host permits creating writer groups in its delegated cgroup v2 hierarchy,
  including migration from the Bubblewrap user namespace.
- The focused native regression passed for pipe, PTY, and direct commands: publication
  excludes new mutations; a detached child with closed standard streams keeps
  publication busy after its leader exits; descendant exit permits publication.
- `just test -p codex-hooks -p codex-utils-pty`: 202 tests passed. The additional kernel
  regression passed with `--run-ignored all -E 'test(writer_admission)'`; it is
  explicitly ignored by default because it requires writable delegation. Direct
  launch waits for publication to finish; failed exec releases admission.
- `cargo check -p codex-core -p codex-hooks --lib` passed using Rust 1.95.0.
  Ambient Rust 1.93 cannot compile this fork's SQLx 0.9 dependencies.
- The subsequent Git/executor pass ran all 41 `codex-git-utils` tests and all
  28 `codex-utils-pty` tests (including delegated-cgroup coverage): passed.
- The core library compiled and 86 tests selected by
  `test(exec::tests) or test(shell_snapshot::tests)` ran: 81 passed initially.
  This Nix host lacks `/bin/bash`. A private Bubblewrap view with `/dev` device
  binding and Bash mounted at `/bin/bash` and `/bin/sh` allowed four of the five
  failed tests to pass, including grandchild timeout cleanup and startup stdin
  isolation. `linux_bash_snapshot_includes_sections` still fails its PATH-export
  assertion. This is unresolved; no claim of a fully passing core selection.
- `just fix -p codex-utils-pty -p codex-hooks`, `just fmt`, and diff whitespace checks passed.
  The subsequent Git/executor pass also passed focused Git/PTY Clippy and formatting.
  Unrelated formatter-only changes to the native justfile were excluded.

## Native control checkpoint

The existing private input-control socket also serves
`POST /v1/workspace/publication`. Request fields are `threadId`, a positive
monotonically increasing `sequence`, and `operation` (`begin` or `finish`).
Responses carry typed `status`: `ready` (with `pid`, `startTicks`, `mountNamespaceInode`, and `cgroupPath`), `settled`,
`busy`, `conflict`, or `unavailable` (with `reason`). Remote app-server connections
and disabled admission are rejected. This does not yet validate remote executor
selection within an in-process runtime; that remains an enablement gate.
Completion requires `expectedIdentity` containing the PID, start ticks, and
mount namespace inode. Begin retries supply it once the host has retained a
receipt. A mismatched identity is rejected before acquiring or releasing admission.

The native admission singleton retains the active sequence and guard independently
of HTTP request or listener lifetime. Repeated begin requests reuse a held lease;
completed sequences cannot begin again; an old finish cannot release a newer
lease. Finish refreshes the original absolute native cwd before reopening writers.
The host must durably record sequence/identity, confirm its mount transition is
settled, then finish. A request timeout alone must not advance the sequence.

The delegated-cgroup test passed with sequence replay/conflict and stale-finish
coverage added. `cargo check -p codex-tui --lib`, focused PTY Clippy, formatting,
and whitespace checks passed. The new HTTP route has not had an end-to-end test.
`just bazel-lock-update` was attempted but not completed: ambient Bazel is absent,
Bazelisk's downloaded 9.0.0 binary cannot start under NixOS's generic ELF-loader
stub, and this Nixpkgs Bazel package is 7.6.0 while the repository requires 9.0.0.
Cargo.lock includes the new Linux-only TUI dependency on the existing PTY crate;
Bazel lock verification remains outstanding.

Shoal's fork-launch path now calls this protocol through `InteractiveAgentBackend`.
`BuildResourceLease` persists the sequence, binding, phase, native receipt, and
prior view record in `native-publication.json` before transitions. Busy or
unsupported admission preserves the latest warm snapshot. The existing host health
loop retries uncertain publications. A lost finish reply retries finish only;
a changed durable view record prevents a second rotation when recording the finish
phase failed. Source capture is not yet connected.
The local publication record is now version 2; unshipped version-1 records are
rejected rather than adopted without process identity. `MountNamespace` compares
start time from the pinned proc directory and the opened namespace inode with the
receipt, then retains pidfd-backed liveness for subsequent host operations.

Focused checks passed: all four build-resource tests, including a real OverlayFS
rotation with a simulated lost native finish reply; and the Unix-socket transport
test for exact identity/sequence and no automatic retry after a lost reply. These
are composed-boundary tests, not a full native-TUI/managed-unfold acceptance run.
Changed agent and host test targets compiled; formatting and whitespace checks
passed. Unrelated codegen formatter churn was excluded.

Automatic publication remains disabled by the native opt-in. Before enabling:
complete host-restart reconstruction of mount-transition evidence. A dead host can still leave
native admission held until ownership is recovered; the native lease alone is not
a completed recovery design.

Publication transport waits now run in the existing child launch tasks. The
deployment retains its build lease through an asynchronous mutex, so launches
from the same creator serialize publication without blocking the fleet loop.
Health recovery acquires that same owner only when idle and runs in an owned
task set; another actor's lifecycle and shutdown events remain selectable during
native HTTP waits. Cancellation can stop a launch waiting for the resource, but
an admitted publication settles before ordinary launch cancellation is observed.
Shutdown timeout and outstanding publication ownership produce unconfirmed
cleanup, preserving storage rather than claiming resource release.

Verification: the Shoal binary compiled; four build-resource checks and three
launch-shutdown checks passed through the owning Nix/Nextest command. These checks
cover publication recovery and existing shutdown accounting; a real multi-actor
run with delayed native control replies has not been performed.

Unknown mount outcomes now retain an `OverlayRecovery` handle in the build
resource owner. Preparing captures the original namespace and mount evidence;
applying consumes the prepared rotation. The retained handle can reconcile or
thaw the original transition but cannot start another rotation. The existing
native-publication retry path uses it before attempting new preparation.
The handle remains in memory: durable reconstitution after losing the host is
still outstanding. Preparation uses a temporary generation directory, retained
only when invoking the mount transition, so repeated pre-mount failures do not
accumulate unused directories.

Checks passed: three owning mount-recovery tests, including delayed/repeated
reconciliation without another mount switch; four build-resource tests, including
preparation-failure cleanup and native finish-reply recovery. Changed host and
node targets compiled; formatting and whitespace checks passed.

## Before enabling

1. Audit remaining native filesystem writers and unsupported executor environments.
   Hosted coordination itself must not hold the mutation gate. See the consumer
   audit below for the remaining Git paths and native state-directory boundary.
2. Complete recovery for the connected native control path. Keep the guard until the host finishes; reconcile disconnects and
   uncertain completion without a timer reopening writes mid-transition.
3. Connect native admission, namespace identity, cwd refresh, source/Git capture,
   and build publication in Shoal. Select the latest warm snapshot independently
   from busy source fallback.
4. Bind cgroup lifetime to host resource custody. The current process-wide static
   owner retains its group; host cleanup/restart reconciliation and reclamation
   are not implemented. Never adopt a stale group merely because a PID repeats.
5. Run native controller and managed-unfold acceptance before updating the runner
   pin or enabling the opt-in. Core integration tests and the full native suite
   have not run for this checkpoint; retain the unresolved shell assertion above.

Kernel contract: [cgroup v2 populated notifications](https://docs.kernel.org/admin-guide/cgroup-v2.html#un-populated-notification)
include live processes throughout the group's descendant hierarchy.

## Consumer audit

The next implementation checkpoint is one complete managed parent/child unfold:
bind admission to the native controller, capture source/Git and build views, and
prove unchanged Cargo reuse. Drive additional writer work from that path rather
than extending the audit to unrelated native subsystems. Recovery, retirement,
fallback, and storage acceptance remain required by the full implementation plan.

- `core/src/spawn.rs` still serves `core/src/exec.rs`; it is a separate local shell
  launch entry from `codex-sandboxing`. Both must participate in admission.
- `git-utils/src/git_process.rs` owns asynchronous metadata commands, including
  fsmonitor probes and queries from `info.rs`. Preserve its existing process-tree
  cleanup while routing process creation through admission.
- `core/src/context/world_state/environment.rs` launches the configured PowerShell
  executable for a version probe. Even this short probe belongs to command custody.
- `git-utils/src/apply.rs` synchronous patch/staging consumers are the separate
  `codex apply` and cloud-task CLI paths, plus tests; the interactive core patch
  tool uses its own already-guarded patch runtime. A CLI invoked by an admitted
  shell remains a descendant of that shell's writer group.
- `operations.rs` synchronous mutations serve the memory baseline in
  `CODEX_HOME/memories`; other callers in `branch.rs` query revision metadata.
  Managed publication must keep native state outside the snapshotted source/build
  roots. Confirm this at launch rather than assuming arbitrary `CODEX_HOME` paths
  are disjoint. The memory service also writes files directly outside Git.
- The TUI's normal app-server client has an in-process implementation sharing the
  native admission singleton. The remote-client variant does not establish local
  execution custody. Snapshot control must bind the actual execution owner and
  reject unsupported remote execution, not infer it from the visible pane.

The identity extension passed the real-overlay host test, including refusal of a
wrong process start time or namespace inode, and the socket test covering initial
begin, identity-bearing begin retries, and identity-bearing finish with both
successful and lost replies. The native admission regression passed and the TUI
library compiled. Formatting and whitespace checks passed in both repositories.
