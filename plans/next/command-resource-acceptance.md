# Command resource isolation acceptance

This slice brings the applications lane's process supervisor onto main and adds
bounded command execution. It does not complete applications A0–A8. The revised
applications checklist separates the implemented supervisor from remaining
delivery, hosted completion, recovery and full resource-settlement work.

## Required behavior

- Full native TUIs, Shoal host and shared compiler remain outside the constrained
  command subtree. Each authored command and its descendants share one allocation.
  External service work, such as builds executed by the Nix daemon, is not a
  descendant of the requesting command and remains outside this allocation.
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

## Checked implementation and release evidence

Tidepool implementation: main `61d6c0b41`; recovery guidance: `bf8bf18f`.
The working checkout is `/tmp/tidepool-rsi-main-20260908`.
Native: `fe15831c8a22c0d1b8d78d5ce55b7aa5fc3fa666`, published on
`inanna-malick/codex:shoal-command-resources` and selected by the flake pin.
The applications supervisor source was selected from `978029124`; its unrelated
native input changes were not imported. Final packaged acceptance passed at runtime source `2b27c3f21474998fe764f87a8ce539290bdeb81c`.

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

## Packaged release

The corrected Nix package built successfully and explicitly supplies systemd
launch tooling on PATH. The full supervised TUI OOM/steering/subsequent-command
fixture passed against its packaged Shoal and native Codex executables (1 test,
1.89 seconds, local scripted provider; no paid inference).

- Runner GC root: `/home/inanna/dev/tidepool/target/resource-runner-final-20260909`.
- Shoal: `/nix/store/ky3y97hfl7f7fp6w0s0w04zhzxw9c3wv-shoal`.
- Native: `/nix/store/wi783qwgz4nrwc014sxn36lxbmj2gh7y-codex-rs-0.0.0-dev+fe15831`.
- Exact source, binary hashes and test result: `selection.json` in the receipt
  directory below; execution output: `checks/packaged-tui.log`.

## Release decision

The operator stopped the full workspace suite and selected targeted mechanism
and adjacent-consumer coverage instead. The interrupted run completed over 1,000
tests without a reported failure, including app-server command-execution cases;
it is not a full-suite pass. Four stale state fixtures were repaired and compiled
before further execution; native test-only commit `240d323124` preserves that fix.
The installed runtime remains `fe15831c` (no production code changed in that fix).

The adjacent selection executed 714 tests: 703 passed, 11 failed. Eight failures
passed after supplying conventional shell paths in an isolated NixOS test view.
Three remote shell-snapshot/sandbox cases remain unverified. The temporary shell
wrapper also broke custom argv0 in an extra recheck; that is not product evidence.
The operator explicitly accepted carrying these fixture issues into a standalone
worker in the next wave rather than blocking launch. Diagnose them with focused
checks; do not call them passed or assume all share the missing-sleep cause.

The command-resource slice is accepted for the next dogfood run with these
recorded test limitations. Full A0–A8 product acceptance remains separate.

The native slice is committed and published, main selects its exact pin, and
scoped formatting/lints and consumer checks are complete. Recovered dirty files
now have exact Git commits; [resume guidance](../parallel-dogfood/next-wave/resume.md)
requires source reconciliation onto the next launch's main before implementation.
Launch acceptance also covers source-checkout process custody and entering the
retained workspace before PID isolation. The retained-view supervisor preserves
device access explicitly: a normal root bind makes `/dev` nodev, so it needs the
same device bind as ordinary launches. All 14 process-scope tests pass, including
a child launched with `/dev/null` stdin and opening a PTY in the retained view.
The initial live planner exposed that device-mount defect before any workers
were commissioned; the replacement runner includes the repair.

Publication capture distinguishes the native-local PID used by begin/finish
receipts from the kernel-authenticated socket peer PID visible to the host.
Both come from the same publication connection; namespace capture still checks
process start time and mount identity. Focused checks cover a real nested PID
namespace, unchanged no-retry behavior after a lost reply, and source/build
inheritance with distinct local and host identities.

Local release receipts and checked logs are retained under
`/home/inanna/dev/tidepool/target/resource-acceptance-20260909/`.
