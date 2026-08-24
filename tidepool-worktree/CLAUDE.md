# tidepool-worktree — managed worktrees, durable registry, typed repository events

**Charter.** Belongs: creating/retaining worktrees, the durable registry,
dirty-source snapshotting, HEAD-movement observation, the event journal, and
the one `git` subprocess call site. Does NOT belong: general git workflow
verbs — rebase, cherry-pick, conflict resolution (stays with coding agents
using their native tools; the one exception is the `merge` primitive
documented below), effect wiring (`tidepool-handlers`'s `WorktreeHandler`).

The Rust substrate for [PRD 19](../plans/self-iterating-harness/19-managed-worktrees-events-prd.md).
Everything here is *git truth*: creating retained worktrees, recording them so a
restart still finds them, snapshotting a dirty source without touching it,
observing HEAD movement, journalling what was observed. No effects, no JIT, no
Haskell, no agents — that separation is what lets every behaviour be tested
against a real temporary repository.

See the root `CLAUDE.md` for the project map.

## Module map

The three **frozen** modules are the shared vocabulary every other module is
written against; changing one is a cross-cutting change, not a local edit.

| Module | Owns |
|---|---|
| `id.rs` | **frozen** — opaque newtypes (`WorktreeId`, `EventId`, `GitOid`, `BranchName`, `GitRef`, `SubscriptionId`) |
| `error.rs` | **frozen** — `WorktreeError` + `DirtySummary` + `GitFailureReceipt` |
| `git.rs` | **frozen** — the ONLY `git` subprocess call site + `inspect::` helpers |
| `registry.rs` | durable `WorktreeReceipt` storage, restart lookup |
| `create.rs` | `WorktreeSpec`/`WorktreeManager` — creation, lookup, listing (incl. the dirty path) |
| `binding.rs` | one worktree, one agent — the binding state machine |
| `snapshot.rs` | temp-index synthetic commit + the untouched-source proof |
| `monitor.rs` | poll/reconcile, coalesced deltas, honest classification |
| `journal.rs` | durable append-only event journal (no replay) |
| `testing.rs` | `ScriptedWriter` — drives a real temp repo with plain git commands, standing in for a coding agent |

## Rules that are not negotiable here

**No general git workflow verbs.** No `rebase`, `cherry_pick`, conflict
RESOLUTION, or branch promotion. PRD 19's boundary is creation, lookup,
inspection, events, plus the one merge primitive below. That work belongs to
coding agents using their native tools, and the runtime observes what the
repository became. Adding a workflow verb beyond the one exception is a
design regression, not a convenience.

**The one narrow, deliberate exception: `merge.rs` (PRD 21 C5), exposed as a
`Worktree` verb.** The recursive companion's worktree-coordination fold —
each node merges its children's worktrees into its own, in declared branch
order (`plans/self-iterating-harness/21-recursive-companion-prd.md`,
"Worktree coordination") — needs ONE typed primitive: merge a branch into a
target worktree, abort-and-report on conflict, never leave a half-merged
tree. `merge::merge_branch_into` is that primitive, through the same
`GitCli` call site as everything else here, with
`MergeOutcome::{Merged,Conflict}` as its typed result (a non-conflict
failure stays the ordinary `WorktreeError::GitFailure`). It IS exposed as a
`Worktree` effect verb — `WorktreeMergeInto` / Haskell `mergeBranchInto`,
generated from `tidepool-protocol`'s schema like every other Worktree verb —
because both `harness-dogfooding/dev-tree/Harness.hs`'s `mergeChild` and
`harness-dogfooding/recursive-companion/Harness.hs`'s `mergeChildInto` had
reimplemented this exact primitive over raw `Exec` and drifted from it: every
nonzero exit read as a conflict, and a failed `merge --abort` was silently
ignored. `merge.rs` exists so that ONE typed, fast-tier-tested definition of
"merge, conflict, abort" is the ground truth every caller uses, instead of
the semantics being re-derived ad hoc at each authored call site. What stays
authored policy on the Haskell side is everything past classification:
resolving a reported conflict, and any git operation this primitive does not
cover (rebase, the boundary/status reads) — both harnesses still reach those
through `gitIn`, now the one shared stdlib helper (`Tidepool.Worktree.gitIn`,
built on `Tidepool.Shell.runInTry`) rather than a per-harness copy.

**Never dirty the source.** The registry root, worktree root, journal, and any
temporary index all live OUTSIDE the source working tree. Managed branches use
`TIDEPOOL_BRANCH_PREFIX`; snapshot commits use `TIDEPOOL_SNAPSHOT_REF_PREFIX`,
deliberately outside `refs/heads/` so they never appear in an operator's
`git branch`.

**Retain first.** No deletion, no GC, no retention policy. A worktree a human
removed by hand becomes `WorktreeError::WorktreeLost`; it is never silently
recreated. Deferred question 1 in the PRD owns any future conversation about
this — it is not an implementation gap to close on your own initiative.

**One `git` call site.** Everything goes through `GitCli`, which scrubs
`GIT_DIR`/`GIT_INDEX_FILE`/`GIT_WORK_TREE` and friends out of the inherited
environment. A module that spawns `Command::new("git")` itself has bypassed the
scrub, the failure-receipt shape, and the temp-index discipline at once.

**Reconciled inspection is the only source of truth.** Not hook payloads, not
filesystem notifications, not an agent's account of what it did. Hooks, when
they exist, are a wake-up that causes a read; they are never the read.

**Observations are coalesced deltas.** The event stream is a sequence of state
deltas, not a movement log. Classification degrades to `UnknownChange` rather
than guessing. Never invent causal attribution to an agent or a model.

**The journal never replays.** A subscription registered now starts at the
journal's current end. Traceability and restart diagnosis read the journal;
handlers do not.

## Testing

Git-behaviour tests run against REAL temporary repositories — `tempfile::TempDir`,
`git init`, real commits — driven by a scripted writer (plain git commands
standing in for a coding agent). Never a mock of git. A mock proves the mock
agrees with your model of git, which is exactly the thing in doubt.

```bash
cargo nextest run -p tidepool-worktree      # pure Rust, no GHC — the fast tier
```

This crate is GHC-free by construction, so it stays in the fast default tier
and needs no `TIDEPOOL_EXTRACT`.
