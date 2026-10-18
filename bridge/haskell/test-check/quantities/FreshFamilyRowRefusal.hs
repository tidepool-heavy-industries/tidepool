{-# LANGUAGE DataKinds, OverloadedStrings #-}
module Main where
import Control.Monad.Freer (Eff)
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import Tidepool.Agent.Ref (AgentRef)
import Tidepool.Actors.Spawn
import Tidepool.Effects.Core (AgentLaunch, ForkAgent)
import Tidepool.Model.Selection
wrong :: Eff '[AgentLaunch, ForkAgent OpenAI] (Either SpawnError AgentRef)
wrong = spawnSubagent (FreshCtx "new conversation") SameDir
  (defaultSpawnOptions (openAI "catalog" "openai" (OpenAIOptions OpenAILow))
    (defaultSpec :: AgentSpec NoTools '[]))
main :: IO ()
main = pure ()
