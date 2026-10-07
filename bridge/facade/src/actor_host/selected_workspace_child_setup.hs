{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Exomonad.Contrib.Types as Types
import qualified Project.Work as Work
import qualified Tidepool.Agent.Contract as A
import Tidepool.Actors.Exomonad

Right selectedTree <- boundWorktree
Right selectedState <- observeSubmission (worktreeId selectedTree)
let selectedTask = Work.task [label|typed-source|] "keep the original task" ["README.md"] "return the same source and obligation" (headOid (submittedHead selectedState))
Right selectedAgent <- spawnSubagent (FreshCtx (Work.taskContext selectedTask))
  (ForkWorktree currentCheckout)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "worker", spawnEffort = Just Medium
    , spawnInstructions = Just (Work.projectPrompt "task"), spawnLifetime = ActorOwned })
Right selectedWorker <- request @(Types.Outcome Types.Candidate) selectedAgent selectedTask
  (defaultRequestOptions { requestLabel = Just "typed-source" })
display True
