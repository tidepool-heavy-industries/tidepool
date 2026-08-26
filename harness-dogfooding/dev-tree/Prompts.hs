{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Prompt grammar and role briefs for the recursive-development-tree
-- dogfood.  This module is the one place typed orchestration becomes prose:
-- shared trust, boundary, checking, and result contracts live beside the
-- worker, recon, planning, microtask, scaffold, integration, resolution, and
-- replan templates that compose them.
module Prompts
  ( sandboxGitContract
  , haskellIdiomContract
  , orchestratorChecksContract
  , boundaryContract
  , workerResultContract
  , workerPrompt
  , proposePrompt
  , reconPrompt
  , microPlanPrompt
  , microPrompt
  , scaffoldPrompt
  , integrationPrompt
  , resolutionPrompt
  , replanPrompt
  , checkLines
  , boundaryLines
  , bulletLines
  ) where

import qualified Data.Text as T
import HarnessTypes
import Tidepool.Prelude
import Tidepool.QQ (fmt)
import Workers (checkFailed)

proposePrompt :: Text -> Budget -> Text -> Text
proposePrompt requestedGoal b grounding = [fmt|
  You are the planning lead for a typed development tree. Propose a DevPlan
  for this goal (which may be PRD-sized — read all of it):
  {requestedGoal}

  Repository grounding (assembled by the orchestrator; trust it over guesses):
{grounding}

  SIZING: cut COARSE. A leaf is a whole coherent deliverable one strong
  worker finishes in one sitting — never a fragment. Mark nodeSplit
  (micro-decomposition) on any leaf whose task names three or more distinct
  components — a split leaf turns "already done?" into a per-microtask
  verdict with its own receipt, where an unsplit one can stall on
  whole-leaf scope judgment.
  Interior nodes exist only when children truly need separate worktrees and
  an integration merge. The root is depth 0; stay at or below depth
  {b.maxDepth}; at most {b.gateWiderThan} direct children per node; respect
  the total allowance of {b.maxAgentCycles} agent cycles (a leaf costs one,
  an interior node two).

  Per node: unique kebab-case nodeName; a self-contained nodeTask a worker
  can act on without this conversation; a tight nodeBoundary (product paths)
  plus nodeTolerated for hygiene paths a worker may touch without failing
  (README, ignore files, lockfiles); nodeChecks as concrete
  orchestrator-runnable shell commands that DISCRIMINATE (exit 0 = success;
  they run harness-side with full tooling, so cargo/cabal/test commands are
  allowed where they fit a 600-second budget); an explicit nodeOnFailure.
  Optionally set nodeCycles on a child that needs more than an equal share
  of the cycle budget (a leaf costs one cycle; an interior node two plus its
  children); omit it everywhere else.

  PATHS ARE VERIFIED, NOT INHERITED: the goal text may be stale relative to
  this tree. Every path you put in a boundary or check must appear in the
  grounding's tracked-file list (or be a directory prefix of entries that
  do). Where the goal names a path the grounding contradicts, follow the
  tree and say so in that node's task. Prefer directory prefixes over exact
  files in boundaries — files move; directories survive refactors.

  CHECK DESIGN: a check verifies the DELIVERABLE, not the diff's text.
  Prefer build/test commands (cargo check -p, cargo nextest run -p, a
  targeted test) over source greps; a grep-shaped check must be specific
  enough not to match unrelated code or its own definition, and negative
  greps ("the old pattern is gone") are a last resort — they break on any
  unrelated occurrence of the pattern.

  CHECK ENVIRONMENT (checks that ignore this fail for reasons unrelated to
  the work): checks run harness-side in a COLD worktree — no warm cargo
  target (a workspace-building command starts from scratch and will not fit
  the 600-second budget), no worker-sandbox generated artifacts, and exactly
  the arguments you write (a script invoked without its required arguments
  is a usage error every time, which discriminates nothing). Every proposed
  check is baseline-run before approval and its baseline exit is shown to
  the operator; a check whose failure cannot be attributed to missing work
  is a malformed check, not a safeguard. If the deliverable's real
  verification is heavier than the budget, say so in the task and give a
  cheap structural check instead — the operator runs the heavy gate at fold.

  SURFACING DECISIONS: this is a multi-round session. If the goal leaves a
  genuine architectural fork only the operator should close, surface it
  BEFORE finalizing: declare a small record in a haskell block —
  data MyDecision = MyDecision {{ choice :: Text, rationale :: Text }}
    deriving stock (Generic)
    deriving anyclass (DerivedForm)
  — then evaluate askUser @MyDecision; the operator answers a typed form and
  your next round sees the value. At most two decisions; never ask what the
  grounding or the goal already answers.
|]

-- | The Git trust boundary shared by workers that edit the assigned worktree.
sandboxGitContract :: Text
sandboxGitContract = [fmt|
  Your sandbox's git directory is read-only: do NOT run `git commit` (or `git add`,
  or any other git write — they will fail). Leave your changes uncommitted; the
  orchestrator snapshots and commits them itself once your turn ends.
|]

-- | Positive style guidance for workers editing Haskell in this repository.
haskellIdiomContract :: Text
haskellIdiomContract = [fmt|
  For Haskell edits, write the idiomatic version first: use records over
  positional argument threading, and use when/unless for pure-unit branches.
  Name your where-helpers. Operator expressions and if-then-else are fine
  inside fmt holes. Custom typeclasses and GADTs compile here, so use them when
  they are the right abstraction.
|]

-- | The checks and compilation boundary shared by orchestrated workers.
--
-- The check lines are already rendered by the caller because node checks and
-- microtask checks come from different sources.
orchestratorChecksContract :: Text -> Text
orchestratorChecksContract renderedCheckLines = [fmt|
  The orchestrator runs these checks itself, in this worktree, after your turn,
  at whatever commit it leaves HEAD on — they are the record, not your summary
  of them. It also owns every compilation gate: your shell has no ghc, so workers
  editing Haskell SHOULD run scripts/worker-typecheck.sh on each edited file
  before finishing; leave direct compilation to the orchestrator:
{renderedCheckLines}
|]

-- | The product-boundary contract, including the tolerated hygiene tier.
boundaryContract :: DevPlan -> Text
boundaryContract p = [fmt|
  Your diff must stay inside these paths (empty means unrestricted); the
  orchestrator diffs your branch against its seed and refuses a fold that
  strays:
{boundaryLines p}
|]

-- | The WorkerResult finishing contract, with a role-specific summary brief.
workerResultContract :: Text -> Text
workerResultContract whatToDescribe = [fmt|
  Finish your turn with a WorkerResult: a one-paragraph workSummary {whatToDescribe},
  an evidence list (commands run, checks passed, files edited — never a commit sha,
  since you did not and cannot commit), and readyForIntegration. Include obstacles:
  what went wrong along the way IN ORDER, even failures you later
  recovered from. Include frictionNotes: anything about this brief, sandbox, or
  checks that made the task harder than it should be, or a tweak you'd request —
  honest, both empty if nothing stood out.
|]

workerPrompt :: DevPlan -> Text
workerPrompt p = [fmt|
  You are the implementation worker for leaf node {nodeName p}.
  Work only in the assigned worktree, using your native edit, shell, test, and
  Git tools.

  Task: {nodeTask p}

{orchestratorChecksContract (checkLines p)}

{boundaryContract p}

  Inspect the repository before editing. Keep your branch buildable and leave
  coherent progress in the working tree.

{sandboxGitContract}

{haskellIdiomContract}

{workerResultContract "describing WHAT you changed"}
|]

reconPrompt :: DevPlan -> SplitSpec -> Text
reconPrompt p spec = [fmt|
  You are a READ-ONLY recon worker. Do not edit, create, or delete any file,
  and do not run any command that writes — you are here to look, not touch.

  A planner is about to split this task into small sequential subtasks, and
  your survey is the only view of the repository it will have:

  Task being planned: {nodeTask p}
  Planning hints: {spec.splitHints}

  Explore the repository with your native read and shell tools. Finish your
  turn with a RepoSurvey: surveyLayout (the repo's shape — layout, languages,
  build and test entry points, in a paragraph), relevantFiles (paths most
  relevant to the task, best first), and surveyRisks (hazards a planner
  should route around: fragile files, missing tooling, surprising
  conventions).
|]

microPlanPrompt :: DevPlan -> SplitSpec -> RepoSurvey -> Text
microPlanPrompt p spec survey = [fmt|
  Split ONE development task into small sequential microtasks. Each microtask
  becomes one short coding-agent cycle in the SAME worktree, run in list
  order — earlier tasks lay foundations later ones build on. This is
  intra-task decomposition: no branches, no new worktrees, one lane.

  Task: {nodeTask p}

  Authored hints for how to cut it up: {spec.splitHints}
  Hard cap: at most {spec.splitMaxTasks} microtasks.

  A read-only recon agent surveyed the repository for you:
  Layout: {survey.surveyLayout}
  Relevant files:
{fileLines}
  Risks:
{riskLines}

  The node's own final acceptance checks (run by the orchestrator after the
  whole sequence — your plan must make these pass):
{checkLines p}

  Answer with a MicroPlan: microtasks (each with a short kebab-case
  microName, a self-contained microInstruction a coding agent can act on
  without seeing this conversation, and microChecks — one or two cheap shell
  commands, run by the orchestrator in the worktree after that cycle, that
  verify THAT microtask landed; exit 0 means pass) and a one-paragraph
  microRationale for the cut you chose.
|]
  where
    fileLines = bulletLines survey.relevantFiles
    riskLines = bulletLines survey.surveyRisks

microPrompt :: DevPlan -> Microtask -> Text
microPrompt p m = [fmt|
  You are one microtask worker in a planned sequence, all working the same
  worktree toward one goal. Earlier microtasks' output is already committed
  in this worktree; later ones build on what you leave.

  Overall task (context only — do NOT do all of it): {nodeTask p}

  YOUR microtask, the only thing to do this cycle: {m.microInstruction}

{orchestratorChecksContract microCheckLines}

{boundaryContract p}

{sandboxGitContract}

{haskellIdiomContract}

{workerResultContract "of WHAT you changed"}
|]
  where
    microCheckLines = bulletLines m.microChecks

scaffoldPrompt :: DevPlan -> [DevPlan] -> Text
scaffoldPrompt p kids = [fmt|
  You are the SCAFFOLD worker for node {nodeName p}. What you leave in the
  working tree is the seam every child below you will be seeded from. The
  orchestrator snapshots your work before it creates the child worktrees.

  Task: {nodeTask p}

  These children will fork from the HEAD the orchestrator leaves after
  committing your work. Write the shared types, stubs, and module boundaries
  they will need; do not implement their work:
{childLines}

{boundaryContract p}

{orchestratorChecksContract (checkLines p)}

{sandboxGitContract}

{workerResultContract "describing WHAT you left in the working tree"}
|]
  where
    childLines =
      T.intercalate "\n" ["  - " <> nodeName k <> ": " <> nodeTask k | k <- kids]

integrationPrompt :: DevPlan -> Int -> [Text] -> [CheckResult] -> Text
integrationPrompt p mergedCount escalationLines checks = [fmt|
  You are the integration worker for node {nodeName p}.

  The orchestrator already merged what it could MECHANICALLY: {mergedCount}
  child branches folded cleanly. It is calling you because the mechanical tier
  left something behind.

  Escalations:
{escLines}

  Checks failing at the current HEAD:
{failLines}

  Inspect every child diff and its test evidence, finish the integration with
  your native Git tools, resolve remaining conflicts by understanding both
  implementations, and run the combined checks. Never discard a child's work
  merely to make the merge easy.

{boundaryContract p}

{sandboxGitContract}

{orchestratorChecksContract integrationCheckLines}

{workerResultContract "describing what you merged and what you ran"}
|]
  where
    escLines = bulletLines escalationLines
    failLines = bulletLines [c.checkCommand <> " (exit " <> show c.checkExit <> ")" | c <- checks, checkFailed c]
    integrationCheckLines = bulletLines [c.checkCommand <> " (exit " <> show c.checkExit <> ")" | c <- checks]

resolutionPrompt :: DevPlan -> Text -> Maybe Text -> Text
resolutionPrompt p onto amendment = [fmt|
  You are an ephemeral rebase-resolution worker for node {nodeName p}.

  Rebase this worktree's branch onto {onto} with your native Git tools. The
  mechanical attempt conflicted and was aborted, so the worktree is clean and
  the rebase is yours to drive from the start.

  Preserve both sides' intent: the base moved because a sibling's work landed,
  and this branch's own commits are not negotiable either. Resolve by
  understanding both, not by taking one side wholesale.
{amendmentBlock}

{boundaryContract p}

{orchestratorChecksContract (checkLines p)}

  Finish your turn with a ResolutionResult: whether you resolved it, what you
  did, and the paths that conflicted.
|]
  where
    amendmentBlock = case amendment of
      Nothing -> "" :: Text
      Just a -> "\n  Additional instruction from the orchestrator: " <> a <> "\n"

replanPrompt :: DevPlan -> DevPlan -> Text -> Text
replanPrompt p child why = [fmt|
  A child of node {nodeName p} failed and its failure policy is Replan.

  Child: {nodeName child}
  Child task: {nodeTask child}
  Failure: {why}

  Decide what the orchestrator should do about this ONE subtree. Answer with a
  ReplanDecision: an amendedInstruction (what a fresh worker should be told
  instead), abandonSubtree (true when no instruction would help), and a short
  rationale. The amendment is journaled either way — a resumed run reads it
  rather than re-asking you.

  If the failure looks MIS-SIZED rather than mis-instructed — the task names
  several distinct components and the worker stalled on scope judgment — set
  amendedSubtree to a replacement DevPlan instead: the failed node re-enters
  as that structure (its root name is forced back to the failed node's own,
  so name it freely). Give each child a concrete deliverable, its own checks,
  and boundary paths within the failed node's boundary; depth and width are
  still bounded by the run's budget. Use Nothing when a rephrased
  instruction is enough — a subtree costs one worktree per child.
|]

checkLines :: DevPlan -> Text
checkLines p = bulletLines (nodeChecks p)

boundaryLines :: DevPlan -> Text
boundaryLines p =
  bulletLines (nodeBoundary p)
    <> "\n  Tolerated paths may be touched as hygiene and are reported informationally, but they are not product paths:\n"
    <> bulletLines (nodeTolerated p)

bulletLines :: [Text] -> Text
bulletLines [] = "  (none)"
bulletLines xs = T.intercalate "\n" (map ("  - " <>) xs)
