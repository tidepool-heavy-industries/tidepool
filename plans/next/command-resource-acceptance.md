# Command resource isolation acceptance

This slice brings the applications lane's process supervisor onto main and adds
bounded command execution. It does not complete applications A0–A8. After this
slice is accepted, revise that checklist against main and the retained lane
checkpoints rather than treating the supervisor as future work.

## Required behavior

- Full native TUIs, Shoal host and shared compiler remain outside the constrained
  command subtree. Each authored command and its descendants share one allocation.
- TOML defaults: two command allocations; each has 8 GiB memory.max
  and 1 GiB swap.max; aggregate limits are 16 GiB memory and 2 GiB swap. There is
  no default memory.high throttle; an explicit optional override remains available.
- Commands queue for at most five minutes. Timeout/cancellation before admission
  means not started; a delayed request cannot revive a cancelled identity.
- Background descendants retain capacity until the cgroup becomes unpopulated.
  Lost waiters and connection failures are not cleanup evidence.
- Actor launches reserve memory headroom before creating launch resources; they
  time out without late creation. No actor-count limit or command classification.
- OOM fails the command, preserves native interactive control and siblings, and
  allows subsequent work. Resource uncertainty cannot become successful execution.
- Workspace publication uses the actor's complete descendant cgroup and remains
  usable after command OOM. Managed admission fails closed; ordinary Codex remains
  usable without Shoal configuration.
- Exact supervisor custody remains in the host lifecycle row. Failed process
  retirement must retain the pane and cleanup capability.

## Current evidence (development checkouts, not a release)

Tidepool: `/tmp/tidepool-rsi-main-20260908`, based on main `fbe9ce4b`.
Native: `fe15831c8a22c0d1b8d78d5ce55b7aa5fc3fa666`, published on
`inanna-malick/codex:shoal-command-resources` and selected by the flake pin.
The applications supervisor source was selected from `978029124`; its unrelated
native input changes were not imported. Final packaged acceptance is pending.

- Twelve host scoped-custody tests passed, including actual supervisor release,
  exact retirement, cancellation/lost results and retained ownership.
- Native PTY, hooks and Git utility checks: 243 tests passed, two explicit live
  fixtures skipped. Scoped `just fix` and `just fmt` completed; the existing core
  `too_many_arguments` warning remains outside this slice.
- Node library: all 56 tests passed, including terminal job control and scope
  failure/cleanup paths.
- `codex-core`, `codex-hooks`, `codex-git-utils` and sandboxing consumers compile.
- Native `just bazel-lock-update` completed with Bazel 9.0.0; no lockfile change.
- Live node test passed: bounded OOM, sibling survival, queue timeout, cancellation
  before admission, abandoned waiter, background descendant custody and reuse.
- Matched host/native test passed over the production Unix HTTP endpoint: pipe and
  PTY cgroup membership, OOM diagnostics/nonzero status, subsequent successful
  commands and restored workspace snapshot admission. Direct wait and captured-output
  consumers also report OOM through the retained receipt.
- Queued HTTP admission is cancelled when the owning host quiesces.
- Full supervised native TUI acceptance passed with a local scripted provider:
  command OOM, ordinary tmux steering, subsequent successful command and exact
  supervisor cleanup. No paid inference was used. The fixture requires the matched
  `codex-code-mode-host` beside `codex` and separates pasted input from Enter.
- The focused native exec selection passed 146 of 157 tests initially. Seven of
  the eleven failures pass with the required FHS command paths; the remaining four
  pass when Bubblewrap is also on PATH. All eleven were rerun explicitly. The
  complete native workspace suite has not run.

The privileged fixtures are explicitly ignored in ordinary test runs. Run them
inside a fresh `systemd-run --user --scope --property=Delegate=yes` scope:

1. Build `tidepool-node --test command_resources` in the repository toolchain;
   run that test binary with `--ignored --exact
   command_oom_and_queue_preserve_the_control_process`.
2. Build native tests using `just test -p codex-utils-pty`. Set
   `SHOAL_NATIVE_RESOURCE_TEST` to its `shoal_resources` integration-test binary.
   Run the Tidepool library test binary with `--ignored --exact
   host_dynamic_tools::resource_tests::matched_native_command_resources`.

Additional boundary checks passed:

- Actual production actor launch: resource timeout and cancellation create no
  runtime directory, hosted endpoint, pane or supervisor launch.
- Actual overlay publication after command OOM: writable handles are released,
  snapshot publication succeeds, the child inherits pre-OOM bytes, and parent and
  child continue independently.

Run delegated tests as the only initial process in their scope. A nextest runner
or compiler daemon left in that scope prevents enabling the memory controller
(`EBUSY`). Build/setup the toolchain first, then launch the selected test binary
through `systemd-run --user --scope -p Delegate=yes`. Native sandbox tests also
require Bubblewrap on PATH and conventional `/bin` paths; use an isolated mount
view on NixOS rather than changing the host filesystem.

## Remaining acceptance

- Freeze the matched runner and verify its actual packaged executables. The
  development full-TUI, CoW and host-launch fixtures have passed.
- Complete scoped consumer compilation, formatting/lints and dependency lockfile
  updates; review the final production diff for duplicate ownership and stale paths.
- Commit the checked native and Tidepool slices, update the pin, freeze a runner,
  and record exact revisions and remaining recovered product branches.
- Preserve recovered dirty work and prepare next-wave branch advancement. Do not
  launch a swarm as part of acceptance.

No helper test or compile-only result proves these remaining items complete.
