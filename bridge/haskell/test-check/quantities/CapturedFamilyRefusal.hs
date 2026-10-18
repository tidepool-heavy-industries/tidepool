{-# LANGUAGE DataKinds, OverloadedStrings #-}
module Main where
import Control.Monad.Freer (Eff)
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import Tidepool.Agent.Ref (AgentRef)
import Tidepool.Actors.Spawn
import Tidepool.Effects.Core (AgentLaunch, ForkAgent)
import Tidepool.Model.Selection
wrong :: ContextCheckpoint OpenAI -> Eff '[AgentLaunch, ForkAgent OpenAI] (Either SpawnError AgentRef)
wrong captured = spawnSubagent (ForkCtx captured) SameDir
  (defaultSpawnOptions (argon "catalog" "argon" (ArgonOptions EffortLow))
    (defaultSpec :: AgentSpec NoTools '[]))
main :: IO ()
main = pure ()
