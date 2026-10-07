{-# LANGUAGE QuasiQuotes #-}
import qualified Tidepool.Actor.Record as R
import Tidepool.Effects.Core (GitRef (..))
import Tidepool.Worktree (workspaceFor)
import qualified Exomonad.Contrib.Merge as Merge
let work = (task ("implement-" <> scenario) "Repair the recovery fixture" ["review-flow.txt"] "One recovery test passes; semantic ownership stays with parent" sourceHead)
      { planPath = "plans/component.md", rationale = "Exercise a counted check before exact review" }
Right workerAgent <- spawnSubagent (FreshCtx (taskContext work))
  (ForkWorktree (atRef (GitRef (renderGitOid sourceHead))))
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just "luna", spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just (taskName work) })
Right (worker, _) <- requestWithProgress @WorkProgress @(Outcome Candidate) workerAgent work defaultRequestOptions
let check = PlanCheck "recovery" ["scripts/cargo-focused-test"] (\oid -> FocusedSpec "recovery fixture" (renderGitOid oid) "fixture" "lib" "fixture::one" 1) (Cmd.MiB 256) WithoutPreparation
integration <- if scenario == "publish" then do
  Right tree <- createWorktree (fromRef (GitRef (renderGitOid sourceHead)) "checked-publish")
  Right workspace <- workspaceFor tree
  merger <- R.start (Merge.mergeInto workspace Nothing ["sh", "-c", "test \"$(cat review-flow.txt)\" = pass"])
  pure (Just (Merge.MergeTarget merger))
  else pure Nothing
let policy = defaultReviewFlowPolicy { flowRepairLimit = 1, flowIntegration = integration }
Right reviewRun <- startReviewFlowWith
  (\_ -> pure (ReviewRouteResult
    (if scenario == "escalate" then EscalateReview "owner scope decision" else HonorReview)
    DeterministicRoute)) me work policy worker [check]
let flow = reviewRunFlow reviewRun
