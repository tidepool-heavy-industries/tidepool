# Native workspace admission checkpoint

Codex branch `work/build-snapshot-admission`, commit `0a202c95df`, in
`/tmp/tidepool-build-snapshot-codex`, based on pinned `d760c5cb8c`.
The Tidepool runner pin and launch environment are unchanged. This is not yet
an enabled or complete native snapshot protocol.

## Implemented boundary

`codex-utils-pty::workspace_admission` owns a shared mutation gate and an exclusively
created Linux writer cgroup. Executor commands enter that group before exec in
both pipe and PTY paths. A task-local scope includes actual executor commands;
ordinary infrastructure process launches are unchanged. `codex-sandboxing` applies
the scope at its common local spawn entry point. The shared patch runtime holds
a mutation guard, including shell-intercepted patches.

Snapshot admission requires the exclusive gate and kernel `populated = 0` on an
opened cgroup events file. It does not infer completion from shell exit, process
group identity, output EOF, or elapsed silence. Detached descendants remain
accounted for. No builds or TUIs are cancelled. Private opt-in environment metadata
is `CODEX_WORKSPACE_SNAPSHOTS`; unavailable cgroup custody disables snapshot
admission while preserving normal native execution.

## Checked

- This host permits creating writer groups in its delegated cgroup v2 hierarchy,
  including migration from the Bubblewrap user namespace.
- The focused native regression passed for pipe and PTY processes: publication
  excludes new mutations; a detached child with closed standard streams keeps
  publication busy after its leader exits; descendant exit permits publication.
- `just test -p codex-utils-pty`: 27 existing tests passed. The additional kernel
  regression passed with `--run-ignored all -E 'test(writer_admission)'`; it is
  explicitly ignored by default because it requires writable delegation.
- `cargo check -p codex-core -p codex-sandboxing --lib` passed using Rust 1.95.0.
  Ambient Rust 1.93 cannot compile this fork's SQLx 0.9 dependencies.
- `just fix -p codex-utils-pty`, `just fmt`, and diff whitespace checks passed.
  Unrelated formatter-only changes to the native justfile were excluded.

## Before enabling

1. Cover remaining process writers, particularly hook commands and shell-snapshot
   initialization, through their owning launch paths. Audit native filesystem
   mutations and unsupported executor environments. Hosted coordination itself
   must not hold the mutation gate.
2. Expose exact admission custody through the native controller/request owner.
   Keep the publication guard until the host finishes; reconcile disconnects and
   uncertain completion without a timer silently reopening writes mid-transition.
3. Connect native admission, namespace identity, cwd refresh, source/Git capture,
   and build publication in Shoal. Select the latest warm snapshot independently
   from busy source fallback.
4. Bind cgroup lifetime to host resource custody. The current process-wide static
   owner retains its group; host cleanup/restart reconciliation and reclamation
   are not implemented. Never adopt a stale group merely because a PID repeats.
5. Run native controller and managed-unfold acceptance before updating the runner
   pin or enabling the opt-in. Core integration tests and the full native suite
   have not run for this checkpoint.

Kernel contract: [cgroup v2 populated notifications](https://docs.kernel.org/admin-guide/cgroup-v2.html#un-populated-notification)
include live processes throughout the group's descendant hierarchy.
