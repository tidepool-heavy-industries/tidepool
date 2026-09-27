# Bounded managed storage

Status: implementation in progress. This is a release obligation for the next
wave, not a claim that import filtering alone fixes disk exhaustion.

## Need and failure chain

Agents need isolated writable source, cheap inherited forks, warm build outputs,
and recoverable work after retirement or host failure. Those requirements do
not require a physical copy of every inherited file for every actor.

The implemented path was:

1. Capture a host tree, including ignored nested development worktrees unless
   explicitly excluded.
2. Import it with `cp --reflink=auto`; on this ext4 filesystem that physically
   copies bytes. Overlay forks subsequently share those layers.
3. Accumulate private source and build changes. Compaction can materialize a
   new merged tree while prior generations remain referenced.
4. At each actor retirement, tar the entire merged source view into an ordinary
   worktree, then detach the mounts. Sharing ends in another physical copy.
5. Retain finalized worktrees and uncertain resources conservatively. The
   existing offline cleanup reports overlays, not the whole retained-worktree
   lifecycle. There is no demonstrated bound on repeated runs' retained bytes.
6. Fill the filesystem; journals, generated Haskell and recovery writes fail
   together. Restart then encounters the independent root-selection defect.

Steps 1, 2 and 4 are verified in owning code. The supplied inventory verifies
large imported trees. Exact historical bytes attributable to each retirement
are unavailable after cache deletion. Wave19 has no observed compaction event.

## Required contracts

### Source is selected once

Use one explicit selection for inventory, admission, copy and drift checks:
tracked files plus ordinary untracked source, recursively handling submodules.
Preserve tracked files despite ignore rules, local edits and deletions, modes,
symlinks and non-UTF8 paths. Never traverse symlinks to import external trees.
Ignored build artifacts require a separate explicit artifact policy; do not
implicitly import nested worktrees, caches or download directories.

### Every materialization has an owner and a budget

Import, compaction and retirement must not independently launch unlimited
copies. Use a shared filesystem-aware admission mechanism at their owning
entry points, with cross-process exclusion/reservation and failure cleanup.
Estimate according to actual sparse/hardlink/copy behavior. Report the planned
bytes, available bytes, reserve, cap and selected strategy before expensive
work. A failed or interrupted copy must not publish a successful receipt.

Free-space checks are not filesystem quotas: external programs and arbitrary
agent commands can still consume the same volume. Document that boundary.
At low space, stop admitting amplification, preserve the source and report a
recoverable refusal. Do not attempt a large cleanup copy to free space.

### Retirement preserves work without eager expansion

Replace unconditional full-tree materialization with durable retained state
and explicit on-demand materialization. Git already owns committed objects and
refs. Preservation must also account for staged versus unstaged content,
untracked files, dirty submodules, metadata and explicitly retained artifacts.
The existing dirty-source synthetic commit is not a drop-in solution: it omits
ignored files, rejects dirty submodules and folds staging distinctions.

Candidate implementation: durable immutable layer custody plus a retired
checkout receipt, with remount/materialization through the existing worktree
owner. Alternative: a Git-backed source snapshot plus separately preserved
index and non-Git content. Choose after auditing all worktree consumers; do
not leave a sparse directory falsely advertised as an ordinary finalized
checkout. Retention must survive process death, not only Arc lifetimes.

### Storage classes and reclamation are explicit

Run journals, receipts, recovery executables and uncommitted source are durable
state. Regenerable compiler/build artifacts are caches. Put durable state under
the state root, with an explicit legacy-path migration policy; moving a path
does not reduce its bytes. Preserve existing runs and fail clearly on ambiguous
ownership rather than silently starting another host.

Extend existing cleanup/reporting to cover finalized/retained worktrees as well
as overlays. Show unique allocated bytes and why a resource is retained.
Reclaim only after proving no live writer, mount or retained reader depends on
it and all required work has durable custody elsewhere. No age-based deletion
of uncommitted work and no blanket Nix/cache cleanup.

### Growth is visible and bounded operationally

Account separately for source baselines, source deltas, retired work, build
artifacts, compiler cache, logs and temporary copies. Repeated waves must not
silently retain another whole source tree per retired actor. Bound regenerable
retention and warn/refuse admission when durable retained work exceeds policy;
durability does not authorize silently deleting valuable work to meet a cap.

## Acceptance evidence

- Physical-copy filesystem: a large ignored nested worktree is not imported;
  tracked and ordinary untracked edits survive, including submodules.
- Concurrent copies cannot each spend the same available budget; interruption
  releases admission and leaves no published partial state.
- Fork and retire several children that each change one small file: retained
  storage growth follows their changes and necessary metadata, not N times the
  full inherited tree. Verify content after host restart.
- Staged/unstaged differences, deletions, symlinks, executable modes, untracked
  files and unsupported preservation cases are retained or explicitly refused.
- A live child retains its parent's layers; the last durable reference permits
  reclamation. Cleanup must not count a request as successful release.
- Inject ENOSPC during import, publication, retirement and journal writing:
  preserve the previous authoritative state; no destructive retry loop.
- Cache removal does not remove the new run's durable work or journals.
- Repeated completed runs with explicit cleanup show bounded disposable storage
  and an honest inventory of intentionally retained work.

## Work ordering

1. Git-aware import selection and bounded copy admission (active lane).
2. Durable-state path and legacy discovery audit; complete consumer map.
3. Retired checkout representation and restore contract; replace eager copies.
4. Apply shared admission to remaining materializations; cleanup/reporting and
   low-space failure paths.
5. Focused failure tests and a small repeated-fork/retire rehearsal before the
   next broad wave. Report remaining external disk-consumer limitations.
