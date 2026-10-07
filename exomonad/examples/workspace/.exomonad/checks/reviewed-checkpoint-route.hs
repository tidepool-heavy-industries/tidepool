{-# LANGUAGE QuasiQuotes #-}
import Tidepool.Effects.Core (GitRef (..))
let work = (task "reviewed-checkpoint" "Review this slice" ["slice.txt"] "read exact source" sourceHead)
      { planPath = "plans/component.md", rationale = "Exercise exact source admission" }
let candidate = Candidate sourceHead [] ["browser gate remains"]
let reviewRequest = ReviewRequest (AssignedTask work) candidate OwnerRepairs
Right reviewerAgent <- spawnSubagent (FreshCtx (reviewContext reviewRequest))
  (ForkWorktree (atRef (GitRef (renderGitOid sourceHead))))
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just "luna", spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "review"), spawnLabel = Just "reviewed-checkpoint" })
Right (reviewer, _) <- requestWithProgress @WorkProgress @(Outcome ReviewDecision)
  reviewerAgent reviewRequest defaultRequestOptions
