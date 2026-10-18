{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Exomonad.Contrib.Types as Types
import qualified Project.Work as Work
import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import qualified Tidepool.Effects as Effects
import Tidepool.Prelude (tshow)
import Tidepool.Actors.Exomonad
import qualified Tidepool.Model.Selection as Selection
import qualified Tidepool.Internal.ModelControl as Control

let fixtureOpenAIEffort options = case Selection.modelSelection currentModel of
      Control.ModelSelectionEnvelope identity key _ -> Selection.openAI identity key options

Right selectedTree <- boundWorktree
Right selectedState <- observeSubmission (worktreeId selectedTree)
let selectedTask = Work.task "typed-source" "keep the original task" ["README.md"] "return the same source and obligation" (headOid (submittedHead selectedState))
selectedSpawn <- spawnSubagent (FreshCtx (Work.taskContext selectedTask))
  (ForkWorktree currentCheckout)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "worker", spawnModel = fixtureOpenAIEffort (Selection.OpenAIOptions Selection.OpenAIMedium)
    , spawnInstructions = Just (Work.projectPrompt "task"), spawnLifetime = ActorOwned })
selectedAgent <- case selectedSpawn of
  Left failure -> Effects.error (tshow failure)
  Right agent -> pure agent
Right selectedWorker <- request @(Types.Outcome Types.Candidate) selectedAgent selectedTask
  (defaultRequestOptions { requestLabel = Just "typed-source" })
display True
