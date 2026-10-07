{-# LANGUAGE DataKinds, OverloadedStrings #-}
module Main where

import Control.Monad (unless)
import Control.Monad.Freer (Eff)
import Tidepool.Agent.Contract
import Tidepool.Duration (seconds)
import Tidepool.Effects (sleep)
import Tidepool.Internal.ActorProfiles (ActorEffects)
import Tidepool.Scope (ScopeOutcome, withScope)

-- Instantiate the spec whose notebook retains the generated general row.
workspaceSpec :: AgentSpec (AsyncHaskellTools ActorEffects) ActorEffects
workspaceSpec = defaultAsyncWorkbenchSpec

scopedSleep :: Eff ActorEffects (ScopeOutcome ())
scopedSleep = withScope (\_ -> sleep (seconds 3))

main :: IO ()
main = do
  installed <- either (error . show) pure (compileInstalledTools (specTools workspaceSpec))
  case map dtdEffectKeys (declarations installed) of
    [Just effects] -> unless ("ResourceScopes" `elem` effects && "Sleep" `elem` effects)
      (error "the concrete workspace notebook lost scope or sleep")
    _ -> error "the concrete workspace spec did not install its async notebook"
