# Workspace inheritance for ordinary unfold

Implement linearly, using the existing Codex TUIs. Start with this design, then
[the ordered tasks](workspace-fork-tasks.md). The
[source review](evidence/build-snapshot-review.md) distinguishes working primitives
from missing integration; [filesystem](evidence/build-snapshots.md) and
[native](evidence/native-workspace-admission.md) notes retain earlier test evidence.

## Contract and scope

Ordinary Haskell unfold inherits the selected live checkout: staged and unstaged
files, deletions, untracked/ignored project files and timestamps. The child has
private HEAD, index and branch. Its source and build paths remain stable inside
the native namespace. No new model-facing snapshot modes or commands are needed.

Root keeps the original checkout and its Git state. Import its source into private
storage for direct children; managed descendants use CoW source layers. Canonical
.shoal and shared Git administration remain separately owned. Designated build
resources, including an existing target used as the root build seed, are excluded
from source import. Preserve other ignored files; do not silently filter by Git
tracking or copy an entire ignored target into each child.

Busy or unavailable source capture falls back to a linked checkout of the selected
parent's current committed HEAD, with a concise notice that working files were
not inherited. Resolve that exact commit rather than invoking the old dirty-source
policy. Explicit ref inputs remain committed inputs.

Build inheritance comes from the orchestration creator, independently of source
selection and conversation context. Use its latest completed warm snapshot when
busy; with none, use a private empty build directory. Never wait for an active
build to finish, cancel it or kill a TUI just to fork.

A Shoal host crash ends the wave. Recover the next wave from Git state, preserving
the original root and committed child branches. Do not restore old live actors,
Haskell state, mount namespaces or publication transactions. Retain uncertain
storage; uncommitted child working files have no automatic crash-recovery promise.
This replaces the earlier requirement for full host-restart reconstruction.

Linux/OverlayFS/Bubblewrap are the target. No filesystem migration, persistent
snapshot service, new TUI, or implementation of the entire application A0-A8 plan
is required here.

## One workspace owner

Keep the existing ownership boundaries:

| Owner | Responsibility |
| --- | --- |
| Actor host workspace custody | One actor's checkout/view/resources; one native publication transaction |
| WorktreeManager and GitCli | Source authorization, private Git state, receipt, host access and checkout mutation exclusion |
| OverlayResourceLease | Immutable generations, private writable layers and their dependency retention |
| ProcessMountBoundary and MountNamespace | Complete view construction, entry, rotation and in-wave reconciliation |
| Native workspace admission | Exclude native mutations, account for command descendants, refresh cwd before reopening writers |
| Existing actor lifecycle/fleet tasks | Bind, launch, finish admitted operations, cancel, retire and report |

Compose the source, optional build resource, namespace and publication state in
one host-owned workspace value. Source storage distinguishes the original
host-backed checkout from a managed layered checkout. Runtime native binding can
arrive later. Keep the generic actor kernel's existing opaque preparation/custody
handoff; it does not need to understand overlays or Codex.

Use checkout identity for filesystem staging, rather than requiring the child
actor ID before it exists. Native policy mounts depend on the already-attenuated
tool class and can be prepared early. Bind the allocated actor in the existing
installer. Do not add another identity issuer, view registry or pending-work map.

## Admission to launch

1. Authorize and resolve the selected source through the worktree owner. Capture
   source from its actual owner. Take the creator's completed build snapshot.
2. Acquire the source workspace's publication exclusion without waiting behind
   another publication. Busy means fallback, not a queue of blocked forks.
   Exclude host-side mutations of this checkout too; the native cgroup does not
   cover Haskell worktree effects. Keep the outer hosted call and fleet unlocked.
3. Begin one exact native publication operation. Under that admission, capture
   source and HEAD/index. When source owner and creator are the same, publish a
   fresh build if possible; otherwise retain the creator's completed build without
   acquiring a second actor's publication gate.
4. Restore/confirm the parent's writable view and finish native admission.
   Release the parent before building the child's namespace: child preparation
   consumes retained immutable source and Git data.
5. Allocate the child's Git administration and private source/build layers,
   assemble one complete view, verify Git identity and finalize its receipt.
   Return that view and its resources through PreparedForkWorkspace.
6. The custody installer binds the child before Haskell entry. Native launch
   enters the prepared view with the existing enter-view path. It does not
   construct another overlay over the same upper/work directories.

Exact-context forks defer actual child startup until the parent's hosted call
completes. Source capture and a usable checkout therefore belong in admission,
not that later startup or native launch. Cancellation after capture may discard a
child; it must not strand the parent gate or release retained parent/child layers.

Root and explicitly prebound launches use the same workspace constructor with
their appropriate source/build inputs. Remove the duplicated late-selection and
view-construction paths once these consumers are connected.

## Filesystem and Git details

The complete view includes source, the child's private .git pointer, canonical
.shoal, the nested private build view and frozen launch-policy mounts. Keep
CODEX_HOME and runtime sockets/logs outside captured project data. Rotation must
preserve separately owned nested mounts.

Initialize the child's root metadata after setting up its private files and
mountpoints, as well as preserving metadata on later rotations. Do not backdate
changed source or disable Cargo validity checks. Reuse one metadata-copy owner.

Original-root import is a real copy on each fresh capture, not an atomic snapshot
against arbitrary external editors. Coordinate owned writers, reject detected
changing/incomplete captures and take the committed fallback. Never repoint root
HEAD/index or silently move its working files. Common Git objects and refs remain
the existing collaboration mechanism, not a new publish/import protocol.

Runtime namespace routing can serve ordinary and layered checkouts. Only a checkout
whose source requires an overlay should durably require that view. After a crash,
read committed branches through common Git and create fresh checkouts; never
pretend an old overlay's host placeholder contains its working files.

## In-wave failure handling

Keep the native replay-safe begin/finish protocol, but move its sequence and
transaction state off individual resources and onto the workspace owner. A
single-request wrapper would not remove the cross-process mount/admission problem.
Keep the existing transport and exact process/namespace identity checks.

| Observation | Behavior |
| --- | --- |
| Source admission busy/unavailable; no mutation started | Current-HEAD source fallback, previous build snapshot, one notice |
| Build publication busy | Keep previous build; source may still succeed if its boundary is available |
| Child setup fails after parent settled | Discard only proven unsubmitted child resources; report failure |
| Lost begin/finish reply | Retry the same operation through its retained owner; never start a new sequence speculatively |
| Mount outcome uncertain | Reconcile the retained transition and establish a writable parent before finishing native admission |
| Shoal host dies | Wave ends; preserve Git and uncertain storage; next wave uses new actors |

Retain in-memory recovery handles before mutation. Caller cancellation or a
transport timeout does not discard admitted work: existing fleet-owned tasks
finish or report it. No timer may reopen writers during an uncertain transition.
Host death may leave an old TUI gate held; automatic recovery of that TUI is
outside the selected Git-recovery contract. It is not silently reused by the next
wave, and this plan does not authorize terminating currently running sessions.

Delete restart-only pending-operation reconstruction and serialized kernel
capabilities once the connected path no longer consumes them. Keep compact
resource/dependency records where actual custody or inspection needs them; these
are not a recovery engine. Old staged manifests are retained, not reinterpreted.

## Warmth and storage

Seed root once from an explicitly selected stable target or build in its private
view before fan-out. A live ordinary target is never an immutable lower. Measure
the initial import cost and path-related invalidation separately from later forks.

Retain layers while workspaces, mounts, operations or descendants reference them.
An actor response or disappearing tmux pane is not cleanup evidence. Reclaim
unused preparation and retired storage only after their existing owners prove it
is unreferenced and no process/host work can use it. Retain uncertain old-wave
storage; automatic crash-orphan garbage collection is outside this wave.

Use flat immutable lower-layer lists and let actual kernel admission determine
capacity. Do not automatically flatten or copy whole build trees at an arbitrary
layer count. Skip publication when the existing owner can prove its upper is
unchanged. A rejected publication preserves the last completed snapshot and uses
the existing source/build fallback without killing useful work. Validate mount
retirement before reclaiming backing files.

## Completion

The production admission-to-launch path handles original root, managed child,
grandchild and busy-source fallback. Real managed TUIs remain interactive while
forking warm workspaces, and changed inputs still rebuild correctly. Measurements
show compilation reuse and physical storage growth, separating source import,
copy-up costs. Cleanup reports retained storage honestly.

Compile/check the matched runner and update the short shipped guidance. Obtain
the user's go-ahead before launching TUIs or a swarm. No live reconstruction
after a Shoal crash is a completion gate.
