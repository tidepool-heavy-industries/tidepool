For Git project implementation and delivery, load `exomonad-project-work`.
It owns scaffold/delegate/review/integrate, recursive Sol/Luna ownership and
acceptance policy. `RECURSIVE-WORK.md` explains the supplied compositions.
This topic covers worktree source selection, observations and role capabilities.

Use `currentCheckout` to select the executing actor's checkout: the root
project checkout or a child's bound checkout. Use `projectHead` for the source
project explicitly. Before a live-source fork,
Exomonad checkpoints eligible edits on the source's current branch. The child starts
from that committed source. If the optional overlay capture is busy or unavailable,
the child starts from the checkpointed HEAD with an omission notice. If native
source admission is busy, it instead uses existing committed HEAD and reports
omitted working files. A Git checkpoint failure stops the fork.

Worktrees. A child's launch receipt names its own checkout, and that path is not
yours to read while the child lives: the directory is mid-edit and may not be
the one your shell resolves. What you may use is the identity — the branch and
the commit — resolved against the repository from your own view. After
settlement the typed result carries the submitted commit and a typed observation
of it (`responseWorktree`, `committedPaths`, `renderGitOid`); with the OID in
hand, ordinary `git show`/`git diff` from your own working directory reads the
content. `createWorktree`, `lookupWorktree`, `boundWorktree` and `listWorktrees`
manage allocated checkouts; `worktreeBranch` and `worktreeHead` read one;
`observeSubmission` types a child's submission; `tryMerge` with a `MergeRequest`
integrates an exact source OID into a managed target worktree, and merges the
OID rather than a branch label so a retained child branch cannot move between
review and fold. `atRef (GitRef "…")` seeds a fork from a deliberate committed
ref; `projectHead` and `currentCheckout` seed it from live source. A commit you cannot
resolve means the child has not checkpointed it yet, not that the work is gone.
Load `exomonad-unfold` for the worked cells.

`tryMerge` integrates a candidate; checks and publication are separate steps.
Keep the exact merged revision and its check evidence for the next action.
A failed check leaves the candidate and local edits available for repair.

Use `coding` for work that may implement and recurse. `scaffolding` has the same
capabilities with a scaffold-focused prompt. `researching` is inspection-only
and may fork research descendants within its runtime budget. `withForkBudget`
requests a bounded subtree; `previewBranch` shows effective policy before launch.
`researchingLeaf`
is an explicit inspection-only leaf. Neither runs builds or tests. Use a coding
actor for a reviewer who must run tests. Explicitly narrowed rows
and exhausted descendant budgets can still make an actor a leaf. Role names
do not replace runtime authority; inspect the `status` tool.

Use `doc unfold` for dispatch and submission observations, and `doc watch` for
composing readiness. Load `exomonad-review` for exact-source review and repair.

skill: exomonad-project-work
