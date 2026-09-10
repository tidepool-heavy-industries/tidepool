# Supervised workspace activation

The launch boundary has two filesystem views. Preparation constructs source,
canonical configuration, and build mounts. The process supervisor creates the
final workload mount/PID namespaces, pins the exact blocked init, and exports
that final view before releasing native execution.

The host activates the final view once. For managed worktrees, activation
replaces exactly the registered preparation view, under the existing registry
lock and Git identity checks. Conflicting or stale activation fails before
release. Native publication must identify the activated view; a matching PID
receipt alone does not select a different workspace.
Publication binds that retained mount authority to the freshly captured native
publisher's descriptors and pidfd. A live namespace exporter cannot substitute
for a live publisher; the focused regression checks their independent lifetimes.

## Mount authority

The final view inherits OverlayFS superblocks owned by preparation's user
namespace. The host retains that preparation authority alongside the final view.
Rotation freezes the final mount, constructs a replacement privately from the
preparation's backing mounts, then grafts it into the final namespace. This
avoids both insufficient superblock authority and locked subordinate mounts in
the workload namespace. Native commands still run without host mount privileges.

Namespace export validates the holder incarnation and exact kernel descriptors
for both views. The private namespace-entry format and supervisor protocol are
version 2. A new runner starts a new wave; live owners are not hot-upgraded.

Mount inspection stays outside the workload PID namespace. Its helper reads
its own mountinfo through retained host procfs, rather than through a procfs
that cannot represent its PID.

## Acceptance

The focused kernel regression now executes rotation in the final supervised
view and then releases a workload that writes there and opens native devices.
The registry regression rejects missing preparation, wrong expected view, and
stale replacement after activation.

`production_tuis_fork_live_workspaces_recursively` launches production Shoal and
real native TUIs against a local scripted Responses provider. No inference is
purchased. It requires recursive Haskell unfold, repeated forks after writes
and a child commit, canonical configuration protection, source mtimes,
untracked inheritance, isolated writes, and fresh inherited Cargo artifacts.
Checks assert successful tool results at the exact scripted calls.

The five-TUI recursive composition passed in 67 seconds, including fresh Cargo
artifacts after both root and managed-child publication. The extended fixture
then runs a busy-source fork (committed files plus the last compiled cache),
command OOM under a 512 MiB limit, and ordinary steering into the same live TUI.
The combined six-TUI acceptance passed in 91 seconds. Quiet snapshot checks
finish before the deliberately busy phase; committed fallback retains compiled
artifacts but can rebuild because checkout mtimes differ.

The extended run also exposed session activation arriving before asynchronous
application deployment. The existing application owner now retains those events
while launch is pending and delivers them in sequence after installation.
Failed or retired applications do not deliver pending activations.

Focused process-boundary checks: 26 passed. Mounted-worktree registry checks:
3 passed. Workspace-owner checks: 2 passed. Scoped-custody checks: 12 passed.
Overlay recovery checks: 3 passed. No full workspace suite was run.

The next paid wave retains planner checkpoint `492de3bb`; it must use a freshly
built, accepted main runner. Product engine/applications branches remain separate.

## Reproduce

Set `SHOAL_RESOURCE_HOST_BIN` to the matched packaged Shoal wrapper, then run:

```sh
nix develop --command cargo test -p tidepool --lib \
  production_tuis_fork_live_workspaces_recursively -- --ignored --nocapture
```

The fixture uses a private local provider, retains failed fixture files for
diagnosis, and retires its owned supervisors/TMUX session. It purchases no
inference. The runner must contain the current supervisor and namespace-entry
protocol; a stale packaged executable does not test current source.
