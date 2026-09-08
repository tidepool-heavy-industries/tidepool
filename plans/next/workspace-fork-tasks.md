# Workspace fork implementation tasks

Read [the selected design](build-snapshot-implementation.md), then the owning
source named by the current task. Work linearly. The primitives exist; do not
restart feasibility research or introduce another scaffolding wave. Names below
describe outcomes, not new model-facing roles or APIs.

Baseline: Tidepool 43ddd89f, native 06d99357be in
/tmp/tidepool-build-snapshot-codex. The native branch is not yet pinned. Preserve
unrelated .shoal, plans/README.md and context-checkpoint work, original Codex
checkout changes, and the stopped dogfood branches. No TUI/swarm launch without
the user's go-ahead.

## 1. Deliver one real original-root to child fork

- [ ] Extend the existing workspace custody owner to retain source, build,
  namespace and one native publication transaction. Move native sequence ownership
  off individual overlay leases. Keep the existing native begin/finish wire path.
  Use live-wave transaction state; do not first extend the old durable journal to
  cover source and then remove it. Task 3 removes remaining unused restart surface.
- [ ] Extend AuthorizedForkWorkspace with live-source preparation and an exact
  current-HEAD fallback through WorktreeManager. Gate the authorized source owner;
  select the creator's cache separately. Do not acquire two actor publication gates.
- [ ] Implement private original-root import under the owned capture boundary,
  with metadata preservation and explicit mount/build exclusions. Preserve source
  HEAD/index. An unstable/unsupported capture produces a typed fallback reason;
  an uncertain mount operation must settle before fallback is considered safe.
- [ ] Assemble/finalize the child's complete view during admission, using checkout
  identity for storage/policy staging. Transfer it through PreparedForkWorkspace;
  the existing installer binds the allocated actor and launch consumes the view.
- [ ] Connect root/prebound launch to the same constructor. Remove the duplicated
  launch-time workspace assembly/build selection where it is superseded.

Owners: tidepool/src/actor_host.rs and its overlay_resource modules;
tidepool-actor/src/fork_workspace.rs and resident_actor.rs;
tidepool-handlers/src/handlers/worktree.rs; tidepool-worktree/src/create.rs,
git.rs and registry.rs; existing ProcessMountBoundary and path owners.

Exit: an ordinary resident Haskell unfold uses real host admission and view
construction with a bounded test backend. Dirty/staged/untracked/ignored root
files reach the child, parent HEAD/index remain unchanged, and a busy source
creates the committed fallback with its selected warm build. Inspect the child's
checkout before deferred native startup; mutate the parent afterward and prove
the child remains at the admitted capture. No manually supplied source layers may
stand in for production admission in this check.

## 2. Make recursive managed forks use the same path

- [ ] Implement managed-source freeze with the shared overlay owner. Preserve
  nested canonical .shoal, private build mounts and launch policy; copy the child's
  private Git pointer and initialize root metadata before finalizing the view.
- [ ] Join source/index capture and the optional fresh build publication under
  the one native gate. Keep a last-completed build on contention; never substitute
  stale source files. Serialize host mutations of the selected checkout too.
- [ ] Keep the existing completed-context fork gate. Source preparation happens
  before it; child Haskell/native execution waits for its normal acknowledgment.
- [ ] Exercise a source owned by another actor, an explicit ref, inspection-only
  children and unavailable native admission without adding model-visible modes.

Exit: the same production fixture unfolds a grandchild from a child whose HEAD,
index and working files differ from root. Parent, child and grandchild commits
are independent. The descendant path performs no eager source/target-tree copy;
busy native writers continue while fallback succeeds. Compile all changed targets.

## 3. Simplify failure handling to the selected wave lifetime

- [ ] Retain typed transition evidence before starting a mount change. A live
  workspace owner retains the exact native operation across cancellation and lost
  replies; existing fleet tasks finish or reconcile it without blocking the fleet.
- [ ] Keep same-sequence replay, exact native identity and cwd refresh. Test lost
  begin, lost finish, failed mount replacement, and child cancellation; each must
  either leave a writable parent or explicitly retain an unresolved operation.
- [ ] Delete restart-only transaction loading, serialized recovery capabilities
  and their fixture-only APIs when no production consumer remains. Keep only
  resource/dependency metadata needed for retention/inspection. Do not reinterpret
  old staged files as a new format or add a migration/replay subsystem.
- [ ] Distinguish live namespace routing from durable overlay dependence.
  Ordinary host-backed Git checkouts must remain usable from Git after the wave.
  Old layered checkouts are not automatically reattached or used as working files.

Exit: ordinary timeout/cancellation paths do not strand native admission or create
a second mount transition. A simulated host loss ends the test wave; a new manager
can recover committed child branches through common Git and create fresh checkouts.
Do not claim recovery of uncommitted child files, old TUIs or live Haskell state.

## 4. Prove useful warmth and bounded storage

- [ ] Connect the explicit stable root target seed, or a warm build in the root
  view. Exclude that resource from source capture; preserve stable native build
  paths and the existing wrapper policy. Never borrow a mutable target as a lower.
- [ ] Use the same production path for an unchanged build, changed local Rust
  source and a changed build-script input. Record actual fresh/rebuilt artifacts
  and distinguish seed-path invalidation from later snapshot reuse.
- [ ] Bound layers and covered mounts. Implement consolidation in the existing
  resource owner over immutable logical contents, outside native write admission.
  Include whiteouts, metadata, failed consolidation and a still-writing parent.
  At capacity, degrade to committed source/previous build instead of unbounded
  growth; no useful build is killed or awaited to satisfy snapshot capacity.
- [ ] Keep dependencies until all workspaces, operations, mounts and children
  release them. Reclaim proven unused preparation and releasable retired storage;
  report unknown custody as retained. Do not use tmux disappearance as proof or
  import the whole application-supervision plan to claim automatic reclamation.

Exit: representative bytes allocated, copy-up and compilation reuse are measured;
repeated publication has a checked bound. A child survives its parent's retirement.
Retention and actual reclamation are reported separately. Crash-orphan GC and
automatic disposal of worktrees retained for review are outside this wave.

## 5. Align guidance and deliver the matched implementation

- [ ] Update the shipped shared guide/examples at the new-wave boundary:
  ordinary unfold inherits working files; explain the concise fallback notice and
  encourage a useful shared build/idle tree before forking without a polling ritual.
  Remove old clean-source/synthetic-snapshot requirements from the ordinary path.
  Keep explicit committed-ref semantics and the existing general worktree effects
  distinct where they still have real consumers.
- [ ] Review native writer coverage for the actual selected launch, including
  unsupported executors and runtime files excluded from source. Use the existing
  focused delegated-cgroup and transport checks; do not repeat broad native suites
  or expand this into a generic writer/supervisor framework.
- [ ] Compile changed consumers, run their focused checks, format and review the
  final diff for obsolete branches/APIs/comments. Preserve unrelated changes.
- [ ] Prepare the matching native pin, runner and bounded acceptance scenario.
  Then obtain the user's go-ahead to launch the real managed TUIs. Check ordinary
  root/child/grandchild unfolding, active-build fallback and continued native input.
  Record exact binaries, source revisions, physical storage and build evidence.

Use the repository's Nix/just recipes for host/Haskell tests; worktree/node tests
are GHC-free. Extend existing fixtures rather than creating a parallel runner.
Report exactly what ran, compiled only, or remains unverified. Before live approval,
the result is prepared for acceptance, not a completed real-TUI validation.
