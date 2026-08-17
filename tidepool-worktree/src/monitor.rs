//! Repository observation — LANE L3.
//!
//! Polling and reconciliation turn "the repository moved" into typed facts.
//! Hooks, when they arrive, are only a wake-up; the source of truth is always a
//! fresh git read, never a hook payload, a filesystem notification, or an
//! agent's account of what it did.
//!
//! ## Coalesced deltas, stated honestly
//!
//! An observer that finds `HEAD` at C after last seeing A reports ONE
//! transition, even if the tree passed through B. This stream is a sequence of
//! state deltas, not a movement log, and no consumer may treat it as exhaustive
//! history. The dependency-propagation job — "children should rebase onto the
//! parent's latest" — needs only latest-state semantics, which coalescing
//! preserves exactly.
//!
//! When the intermediate history is not recoverable, classification degrades to
//! [`HeadChangeKind::UnknownChange`]. That is a correct answer, not a failure:
//! inventing `Advanced` for a movement that was actually a reset would send a
//! child rebasing onto a commit that no longer means what the classification
//! claimed.
//!
//! ## Honest `commit` classification
//!
//! [`RepositoryEvent::Commit`] is emitted only when a commit can be honestly
//! inferred from git state. The monitor NEVER attributes causality to an agent
//! or a model — it reports what the repository became, and the agent receipts
//! from PRD 18 separately report what the harness observed a worker doing.
//! Those two are complementary evidence; conflating them would let a worker's
//! prose become proof of a commit.
//!
//! A `commit` observation is emitted for `Advanced` (once per commit gained —
//! each is independently, honestly inferable even when several are coalesced
//! into one `HeadChanged`) and for `Amended` (the replacement tip is a real
//! commit object). `Rewound`, `Switched`, `Rewritten`, and `UnknownChange`
//! never carry a co-emitted `Commit`: a reset or checkout creates no new
//! commit, and a rebase's synthetic commits are not "a commit the user made in
//! this worktree" in the sense the PRD's review/test/receipt consumers expect.
//!
//! ## First observation of a worktree
//!
//! [`WorktreeMonitor::register`] establishes the starting baseline itself
//! (recovered from the journal on restart, or a fresh git read otherwise)
//! before `reconcile` is ever called for that worktree. No `HeadChanged` is
//! emitted for that priming: there is no prior state for the worktree to have
//! changed FROM, so there is nothing to report yet, and honest classification
//! has no kind that means "first sight" — see `L3-receipt.md` for the reasoning.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::WorktreeError;
use crate::git::GitCli;
use crate::id::{BranchName, EventId, GitOid, WorktreeId};
use crate::journal::EventJournal;
use crate::storage::now_ms;

/// Default interval between backstop reconciliation polls, in milliseconds.
///
/// Not enforced by this crate: [`WorktreeMonitor::reconcile`] is a single
/// pass, driven externally. The timer loop that calls it on a schedule
/// belongs to whichever realm/driver owns process scheduling. This constant
/// is that loop's recommended DEFAULT, named and exported so a caller can
/// override it rather than the number being buried as a magic literal
/// wherever the loop eventually lives — its *value* is tunable, that it
/// exists is not.
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 5_000;

/// An observation, carrying the runtime identity that ties co-emitted views of
/// one change together.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observed<T> {
    pub event_id: EventId,
    pub value: T,
}

/// How `HEAD` moved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeadChangeKind {
    /// Fast-forward: the old head is an ancestor of the new one. Carries the
    /// commits gained, oldest first.
    Advanced(Vec<GitOid>),
    /// The tip commit was replaced by one with the same parent(s): `(old, new)`.
    Amended(GitOid, GitOid),
    /// History was rewritten: old/new pairs where they could be matched up.
    Rewritten(Vec<(GitOid, GitOid)>),
    /// The new head is an ancestor of the old one.
    Rewound,
    /// The worktree changed which branch it is on.
    Switched,
    /// The movement is real but its shape is not honestly recoverable.
    UnknownChange,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadChangeReceipt {
    pub worktree: WorktreeId,
    /// `None` would describe a report with no prior baseline. This
    /// implementation never constructs one: [`WorktreeMonitor::register`]
    /// establishes the starting baseline silently (see the module docs), so a
    /// `HeadChanged` is only ever emitted for a genuine transition between two
    /// known states.
    pub old_head: Option<GitOid>,
    pub new_head: GitOid,
    pub kind: HeadChangeKind,
    /// `None` on a detached HEAD.
    pub branch: Option<BranchName>,
    pub observed_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitReceipt {
    pub worktree: WorktreeId,
    pub oid: GitOid,
    pub parents: Vec<GitOid>,
    pub subject: String,
    pub author: String,
    pub committed_at_ms: i64,
    /// Repository-relative paths the commit touched.
    pub files: Vec<String>,
}

/// One reconciled repository fact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepositoryEvent {
    HeadChanged(HeadChangeReceipt),
    Commit(CommitReceipt),
}

impl RepositoryEvent {
    pub fn worktree(&self) -> &WorktreeId {
        match self {
            RepositoryEvent::HeadChanged(r) => &r.worktree,
            RepositoryEvent::Commit(r) => &r.worktree,
        }
    }
}

/// Last-observed state for one watched worktree. Legitimate state — a delta
/// needs a previous — established by [`WorktreeMonitor::register`] and
/// advanced by every non-empty [`WorktreeMonitor::reconcile`].
#[derive(Clone, Debug)]
struct Baseline {
    path: PathBuf,
    head: Option<GitOid>,
    branch: Option<BranchName>,
}

/// Watches one or more managed worktrees.
///
/// The monitor holds the LAST OBSERVED state per worktree — that is legitimate
/// state (a delta needs a previous), unlike caching git facts, which is not.
/// It must survive restart by re-reading its last journalled observation rather
/// than by assuming an in-memory baseline; see [`WorktreeMonitor::register`].
#[derive(Debug)]
pub struct WorktreeMonitor {
    git: GitCli,
    journal: EventJournal,
    baselines: HashMap<WorktreeId, Baseline>,
    next_event_seq: u64,
}

impl WorktreeMonitor {
    /// `journal` is durable, shared across every worktree this monitor
    /// watches — cursor semantics (no-replay, restart recovery) are defined
    /// against one journal end, not one per worktree.
    pub fn new(git: GitCli, journal: EventJournal) -> Self {
        let next_event_seq = journal
            .since(0)
            .expect("EventJournal::since never fails")
            .iter()
            .map(|e| e.event_id.0)
            .max()
            .map_or(1, |max| max + 1);
        Self {
            git,
            journal,
            baselines: HashMap::new(),
            next_event_seq,
        }
    }

    /// Start watching `worktree` at `path`. Establishes the starting baseline
    /// — from the journal's last observation of this worktree if one exists
    /// (restart recovery: a movement that happened while the process was down
    /// must still be reported on the next `reconcile`), otherwise from a fresh
    /// git read (true first sight: nothing to report yet, so nothing is
    /// journalled here). See the module docs for why no event is emitted.
    pub fn register(&mut self, worktree: WorktreeId, path: PathBuf) -> Result<(), WorktreeError> {
        let (mut head, mut branch) = self.last_observed(&worktree);
        if head.is_none() {
            if !crate::registry::worktree_present(&path) {
                return Err(WorktreeError::WorktreeLost(worktree));
            }
            head = Some(read_head(&self.git, &path)?);
            branch = read_branch(&self.git, &path);
        }
        self.baselines
            .insert(worktree, Baseline { path, head, branch });
        Ok(())
    }

    fn last_observed(&self, worktree: &WorktreeId) -> (Option<GitOid>, Option<BranchName>) {
        let entries = self
            .journal
            .since(0)
            .expect("EventJournal::since never fails");
        entries
            .into_iter()
            .rev()
            .find_map(|entry| match entry.event {
                RepositoryEvent::HeadChanged(r) if &r.worktree == worktree => {
                    Some((Some(r.new_head), r.branch))
                }
                _ => None,
            })
            .unwrap_or((None, None))
    }

    /// Reconcile one worktree against its last observed state and return the
    /// facts that follow, in observation order, each carrying the
    /// [`EventId`] the pass minted and journalled — so the id a caller holds
    /// is provably the id a restart diagnosis finds in the journal, sharing
    /// one id when they describe one underlying change. Returns empty when
    /// nothing moved.
    ///
    /// Idempotent: reconciling twice with no writer in between yields nothing
    /// the second time.
    ///
    /// `Err(WorktreeError::WorktreeNotRegistered)` when `register` never ran
    /// for `worktree` — a typed failure an author can match on, not a panic,
    /// since worktree ids reach this call from author-supplied values at the
    /// effect surface. `Err(WorktreeError::WorktreeLost)` when `worktree` WAS
    /// registered but its path is gone from disk (a human removed it,
    /// retain-first's "never silently recreated" case), rather than letting
    /// the subsequent `git` invocation fail opaquely against a missing
    /// directory.
    pub fn reconcile(
        &mut self,
        worktree: &WorktreeId,
    ) -> Result<Vec<Observed<RepositoryEvent>>, WorktreeError> {
        let baseline = self
            .baselines
            .get(worktree)
            .ok_or_else(|| WorktreeError::WorktreeNotRegistered(worktree.clone()))?;
        if !crate::registry::worktree_present(&baseline.path) {
            return Err(WorktreeError::WorktreeLost(worktree.clone()));
        }
        let path = baseline.path.clone();
        let old_head = baseline
            .head
            .clone()
            .expect("register always establishes a concrete baseline before reconcile runs");
        let old_branch = baseline.branch.clone();

        let new_head = read_head(&self.git, &path)?;
        let new_branch = read_branch(&self.git, &path);

        if old_head == new_head && old_branch == new_branch {
            return Ok(Vec::new());
        }

        let event_id = self.mint_event_id();
        let observed_at_ms = now_ms();

        let kind = if old_branch != new_branch {
            HeadChangeKind::Switched
        } else {
            classify(&self.git, &path, &old_head, &new_head)
        };

        // Build the WHOLE batch before the first journal write: receipt
        // construction is the failure-prone half (a git read per gained oid),
        // and failing after a partial journalling would leave rows behind
        // that a retry could not tell from new work.
        let mut batch: Vec<RepositoryEvent> = Vec::new();
        match &kind {
            HeadChangeKind::Advanced(gained) => {
                for oid in gained {
                    let receipt = build_commit_receipt(&self.git, &path, worktree.clone(), oid)?;
                    batch.push(RepositoryEvent::Commit(receipt));
                }
            }
            HeadChangeKind::Amended(_, new) => {
                let receipt = build_commit_receipt(&self.git, &path, worktree.clone(), new)?;
                batch.push(RepositoryEvent::Commit(receipt));
            }
            HeadChangeKind::Rewritten(_)
            | HeadChangeKind::Rewound
            | HeadChangeKind::Switched
            | HeadChangeKind::UnknownChange => {}
        }
        batch.push(RepositoryEvent::HeadChanged(HeadChangeReceipt {
            worktree: worktree.clone(),
            old_head: Some(old_head),
            new_head: new_head.clone(),
            kind,
            branch: new_branch.clone(),
            observed_at_ms,
        }));

        // Append idempotently. A pass that failed mid-batch retained its old
        // baseline, so the retry rebuilds the same observations — any row the
        // failed pass already journalled is DELIVERED again (the failed pass
        // returned Err, so subscribers never saw it) but NOT re-journalled,
        // and it keeps its journalled EventId so id-based dedup downstream
        // stays sound.
        //
        // The dedup window is NOT the whole journal — a transition can
        // legitimately recur (A -> B, rewound, A -> B again) and a full-history
        // match would swallow the genuine second observation. It is exactly
        // the rows a failed pass can leave: `HeadChanged` is appended last and
        // the baseline insert after it is infallible, so a pass that
        // journalled its `HeadChanged` always advanced the baseline and is
        // not being retried. Leftovers are therefore only COMMIT rows for
        // this worktree sitting AFTER this worktree's last `HeadChanged`.
        let worktree_key = worktree.clone();
        let leftover_commits: Vec<(GitOid, EventId)> = self
            .journal
            .iter()
            .rev()
            .take_while(|entry| {
                !matches!(&entry.event, RepositoryEvent::HeadChanged(h) if h.worktree == worktree_key)
            })
            .filter_map(|entry| match &entry.event {
                RepositoryEvent::Commit(c) if c.worktree == worktree_key => {
                    Some((c.oid.clone(), entry.event_id))
                }
                _ => None,
            })
            .collect();

        let mut events = Vec::new();
        for ev in batch {
            let prior_id = match &ev {
                RepositoryEvent::Commit(c) => leftover_commits
                    .iter()
                    .find(|(oid, _)| *oid == c.oid)
                    .map(|(_, id)| *id),
                RepositoryEvent::HeadChanged(_) => None,
            };
            match prior_id {
                Some(prior_id) => events.push(Observed {
                    event_id: prior_id,
                    value: ev,
                }),
                None => {
                    self.journal.append(&ev, event_id)?;
                    events.push(Observed {
                        event_id,
                        value: ev,
                    });
                }
            }
        }

        self.baselines.insert(
            worktree.clone(),
            Baseline {
                path,
                head: Some(new_head),
                branch: new_branch,
            },
        );

        Ok(events)
    }

    fn mint_event_id(&mut self) -> EventId {
        let id = EventId(self.next_event_seq);
        self.next_event_seq += 1;
        id
    }
}

fn read_head(git: &GitCli, cwd: &Path) -> Result<GitOid, WorktreeError> {
    let out = git.try_run(cwd, &["rev-parse", "HEAD"])?;
    Ok(GitOid::from_raw(out.trimmed()))
}

fn read_branch(git: &GitCli, cwd: &Path) -> Option<BranchName> {
    git.run(cwd, &["symbolic-ref", "--short", "HEAD"])
        .ok()
        .map(|out| BranchName::from_raw(out.trimmed()))
}

/// Whether `ancestor` is an ancestor of `descendant`, honestly distinguishing
/// "no" from "cannot tell". `merge-base --is-ancestor` exits 1 for a genuine
/// "not an ancestor" and something else (commonly 128, "not a valid object")
/// when an endpoint is not resolvable at all — e.g. after the old head was
/// garbage-collected. Collapsing those two into the same answer would let
/// classification carry on to wrongly conclude `Rewound`/`Rewritten` from an
/// unresolvable object.
enum Ancestry {
    Yes,
    No,
    Unknown,
}

fn is_ancestor(git: &GitCli, cwd: &Path, ancestor: &GitOid, descendant: &GitOid) -> Ancestry {
    match git.run(
        cwd,
        &[
            "merge-base",
            "--is-ancestor",
            ancestor.as_str(),
            descendant.as_str(),
        ],
    ) {
        Ok(_) => Ancestry::Yes,
        Err(receipt) if receipt.exit_code == Some(1) => Ancestry::No,
        Err(_) => Ancestry::Unknown,
    }
}

/// `oid`'s parents, or `None` if `oid` cannot be resolved at all (e.g. it was
/// garbage-collected out from under a stale baseline).
fn parents_of(git: &GitCli, cwd: &Path, oid: &GitOid) -> Option<Vec<GitOid>> {
    let out = git
        .run(cwd, &["rev-list", "--parents", "-n", "1", oid.as_str()])
        .ok()?;
    let mut parts = out.trimmed().split_whitespace();
    parts.next()?; // oid itself
    Some(parts.map(GitOid::from_raw).collect())
}

/// Commits reachable from `tip` but not from `exclude`, oldest first, as
/// `(oid, subject)`. `None` if the range cannot be resolved.
fn commits_only_in(
    git: &GitCli,
    cwd: &Path,
    exclude: &GitOid,
    tip: &GitOid,
) -> Option<Vec<(GitOid, String)>> {
    let range = format!("{}..{}", exclude.as_str(), tip.as_str());
    let out = git
        .run(
            cwd,
            &["log", "--reverse", "--format=%H%x1f%s", range.as_str()],
        )
        .ok()?;
    Some(
        out.lines()
            .into_iter()
            .filter_map(|line| {
                let mut parts = line.splitn(2, '\u{1f}');
                let oid = parts.next()?;
                let subject = parts.next().unwrap_or_default().to_string();
                Some((GitOid::from_raw(oid), subject))
            })
            .collect(),
    )
}

/// Match every commit in `old_only` (in order) to a same-subject commit in
/// `new_only`, allowing unmatched `new_only` commits (upstream content picked
/// up by the rebase) to sit between matches. This is the honest shape of "a
/// rebase preserves messages, changes parents": it recognizes the pairing
/// without requiring the two lists to be the same length, which they almost
/// never are once the upstream has moved. `None` if any `old_only` commit has
/// no match, or if `old_only` is empty (nothing to have been rewritten).
fn match_rewritten(
    old_only: &[(GitOid, String)],
    new_only: &[(GitOid, String)],
) -> Option<Vec<(GitOid, GitOid)>> {
    if old_only.is_empty() {
        return None;
    }
    let mut pairs = Vec::with_capacity(old_only.len());
    let mut cursor = 0usize;
    for (old_oid, subject) in old_only {
        let offset = new_only[cursor..].iter().position(|(_, s)| s == subject)?;
        let idx = cursor + offset;
        pairs.push((old_oid.clone(), new_only[idx].0.clone()));
        cursor = idx + 1;
    }
    Some(pairs)
}

/// Classify a `HEAD` movement from `old` to `new` at `cwd`, given the branch
/// name did not change (a branch change is classified `Switched` by the
/// caller before this runs, unconditionally — see the module docs on why that
/// takes priority). Never errors: an ambiguous or unresolvable movement
/// degrades to [`HeadChangeKind::UnknownChange`] rather than propagating a
/// git failure, because "the shape is not recoverable" is this function's
/// correct answer, not its failure mode.
fn classify(git: &GitCli, cwd: &Path, old: &GitOid, new: &GitOid) -> HeadChangeKind {
    // Amend: the tip was replaced by a commit with the exact same parent set.
    // Checked first and without any ancestor walk, because it is a precise,
    // cheap signature independent of whether the message also changed.
    if let (Some(old_parents), Some(new_parents)) =
        (parents_of(git, cwd, old), parents_of(git, cwd, new))
    {
        if old_parents == new_parents {
            return HeadChangeKind::Amended(old.clone(), new.clone());
        }
    }

    match is_ancestor(git, cwd, old, new) {
        Ancestry::Yes => {
            return match commits_only_in(git, cwd, old, new) {
                Some(gained) if !gained.is_empty() => {
                    HeadChangeKind::Advanced(gained.into_iter().map(|(oid, _)| oid).collect())
                }
                _ => HeadChangeKind::UnknownChange,
            };
        }
        Ancestry::Unknown => return HeadChangeKind::UnknownChange,
        Ancestry::No => {}
    }

    match is_ancestor(git, cwd, new, old) {
        Ancestry::Yes => return HeadChangeKind::Rewound,
        Ancestry::Unknown => return HeadChangeKind::UnknownChange,
        Ancestry::No => {}
    }

    let old_only = commits_only_in(git, cwd, new, old);
    let new_only = commits_only_in(git, cwd, old, new);
    match (old_only, new_only) {
        (Some(old_only), Some(new_only)) => match match_rewritten(&old_only, &new_only) {
            Some(pairs) => HeadChangeKind::Rewritten(pairs),
            None => HeadChangeKind::UnknownChange,
        },
        _ => HeadChangeKind::UnknownChange,
    }
}

fn build_commit_receipt(
    git: &GitCli,
    cwd: &Path,
    worktree: WorktreeId,
    oid: &GitOid,
) -> Result<CommitReceipt, WorktreeError> {
    let meta = git.try_run(
        cwd,
        &[
            "log",
            "-n",
            "1",
            "--format=%H%x1f%P%x1f%an <%ae>%x1f%ct%x1f%s",
            oid.as_str(),
        ],
    )?;
    let line = meta.trimmed();
    let mut parts = line.splitn(5, '\u{1f}');
    let _hash = parts.next().unwrap_or_default();
    let parents_field = parts.next().unwrap_or_default();
    let author = parts.next().unwrap_or_default().to_string();
    let committed_at_s: i64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let subject = parts.next().unwrap_or_default().to_string();

    let parents = parents_field
        .split_whitespace()
        .map(GitOid::from_raw)
        .collect();

    let files_out = git.try_run(
        cwd,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "--root",
            oid.as_str(),
        ],
    )?;
    let files = files_out.lines().into_iter().map(String::from).collect();

    Ok(CommitReceipt {
        worktree,
        oid: oid.clone(),
        parents,
        subject,
        author,
        committed_at_ms: committed_at_s * 1000,
        files,
    })
}
