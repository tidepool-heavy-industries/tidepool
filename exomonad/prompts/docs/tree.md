Scaffold / fork / fold / repeat keeps shared understanding in a coordinator
while specialists retain implementation detail. Fork when the next work is
independent and would otherwise fill your context with unrelated histories.
A mature shared prefix is useful even when large; cache reuse is observed,
not guaranteed. Choose depth and width from the actual obligations.

Commit a useful interface, example, test, or partial implementation in your
owned worktree. Name each obligation's scope, acceptance condition, and allowed
holes. Use `boundHead` for an allocated child checkout and `projectHead` for the source
project. The hosted root writes the project checkout directly and has no bound
worktree handle: seed its children with `projectHead`. Before a live-source fork,
Exomonad checkpoints eligible edits on the source's current branch. The child starts
from that committed source. If the optional overlay capture is busy or unavailable,
the child starts from the checkpointed HEAD with an omission notice. If native
source admission is busy, it instead uses existing committed HEAD and reports
omitted working files. A Git checkpoint failure stops the fork.
Commit authored units with meaningful
messages for integration and recovery.

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
ref; `projectHead` and `boundHead` seed it from live source. A commit you cannot
resolve means the child has not checkpointed it yet, not that the work is gone.
Load `exomonad-unfold` for the worked cells.

`tryMerge` is the primitive, not the whole job. Merging a candidate means
merging it, running the check, and putting the worktree back if the check goes
red. Before writing that sequence by hand, find out whether an installed actor
already fits: `doc topics` ends by naming this workspace's own compiled modules,
and `lookup` on one of those names browses its declarations and the outcomes it
can return. Many workspaces author none, in which case writing the sequence is
the right answer — the point is to know which case you are in before you spend
turns. One lead browsed a module, saw its red-rollback outcome in the answer,
and still spent nine model turns rebuilding it in shell; starting that actor was
four calls, and the hand-rolled version proved nothing about the mechanism it
replaced.

One shared interface can support four branches: a pure test implementation,
integration tests exercising the real implementation, the real implementation,
and code using it. A testing branch may scaffold common fixtures and fork three
responsibilities such as normal behavior, failures, and cleanup. Each parent
owns its shared wiring and fulfills its own contract after folding its children.

Use `coding` for work that may implement and recurse. `scaffolding` has the same
capabilities with a scaffold-focused prompt. `researching` is inspection-only
and may fork research descendants within its runtime budget. `withForkBudget`
requests a bounded subtree; `previewBranch` shows effective policy before launch.
`researchingLeaf`
is an explicit inspection-only leaf. Neither runs builds or tests. Use a coding
actor for a reviewer who must run tests. Explicitly narrowed rows
and exhausted descendant budgets can still make an actor a leaf. Role names
do not replace runtime authority; inspect the `status` tool.

Compile-only fragments and tests that fail against an explicit stub can be
valid intermediate submissions. Record exactly what passed, only compiled,
failed, or remains unimplemented. Removing a TODO marker is not acceptance.
Children may introduce internal obligations but cannot weaken the contract owed
to their parent. Return a typed decision need when that contract must change.

Use `lookup` with `doc unfold` for dispatch and `doc watch` for observation. Watch independent
submissions separately when you can integrate them separately. A fold includes
your judgment: inspect exact commits, validate claims, integrate through ordinary
Git or the conservative merge, and run checks for the integrated revision.
Retain discoveries that change the shared design, not every debugging exchange.

A reviewer forked after implementation returns inherits your newer context.
Supply the exact candidate, issued contract, and implementer reference. The
reviewer can drive typed repairs directly while your watch waits for a verdict.
Use `lookup` with `doc refinement`. Integration and contract changes remain your decisions.

Keep specialists for focused follow-ups; send the new candidate and decision
delta. Fork again when your newer context is the better starting point. A
coordinator can keep its original request pending across watches and finally
use its original `respond`. Acceptance does not require discarding useful actors.

Improve the environment during useful work. Ordinary scripts, resident helpers,
and parameterized acceptance functions can make the next cycle easier. Prefer
existing tools and small project definitions; promote source when actual use
justifies it. No generic campaign schema or mandatory experiment is needed.

Research can deliver source-controlled knowledge. Use a coding actor when the
owned deliverable is a recommendation document: assign each specialist a distinct
path and the coordinator the synthesis. Ask for a recommendation, evidence,
alternatives, uncertainties, and implementation consequences. Restricting that
assignment to documents is a task constraint, not enforced read-only authority;
choose inspection-only research when that authority boundary is needed. Its
coordinator folds reported findings into shared notes, preserving qualifications.

When an interface changes, its owner commits the revised contract and sends each
affected retained actor the revision and decision delta. Track which contract
revision each candidate satisfies; a reply against the old seed is not evidence
for the new contract. Ordinary typed assignment and result values suffice.

Distinguish findings, completed deliverables, and choices requiring the parent.
For a decision request, state the exact choice and what work can continue. Keep
minor friction in the folded notes rather than turning every observation into a
user interruption. Verify subtree activity before reporting a pause: a returned
request, retained actor, pending watch, and working descendant are distinct.
