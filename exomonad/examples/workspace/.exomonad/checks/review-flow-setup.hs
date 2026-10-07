{-# LANGUAGE QuasiQuotes #-}
import Tidepool.Effects.Core (GitRef (..))
let work = (task "implementation" "Implement the feature" ["feature.txt"] "read exact feature" sourceHead)
      { planPath = "plans/component.md", rationale = "Review the settled candidate without a model relay" }
let input = ReviewRequest (AssignedTask work) (Candidate sourceHead [] ["implementation pending"]) OwnerRepairs
Right reviewerAgent <- spawnSubagent (FreshCtx (reviewContext input))
  (ForkWorktree (atRef (GitRef (renderGitOid sourceHead))))
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just (Alias "luna"), spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "review"), spawnLabel = Just "review-produced-candidate" })
Right (reviewer, initialReviewProgress) <- requestWithProgress @WorkProgress @(Outcome ReviewDecision)
  reviewerAgent input defaultRequestOptions
