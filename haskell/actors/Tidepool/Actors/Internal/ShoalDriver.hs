{-# LANGUAGE OverloadedStrings #-}

-- | Private interactive-runtime trampoline.
--
-- This is not Shoal's actor program. It only hands each model-authored
-- 'AgentAction' to the resident interpreter and reopens the same hosted
-- workbench when that action settles.
module Tidepool.Actors.Internal.ShoalDriver
  ( RootEffects
  , rootDriver
  , module Tidepool.Actors.Shoal
  ) where

import Control.Monad.Freer (Eff)
import Prelude

import Tidepool.Actor
import Tidepool.Agent.Action
import Tidepool.Agent.Session (agentSession)
import Tidepool.Actors.Shoal

type RootEffects = ActorEffects

rootDriver :: Eff RootEffects a
rootDriver = loop Nothing Nothing
  where
    loop initialUser interruption = do
      action <-
        ( agentSession initialUser interruption
            :: Eff RootEffects (AgentAction RootEffects ())
        )
      outcome <- runAgentAction action
      case outcome of
        Right () -> loop Nothing Nothing
        Left failure ->
          loop
            (Just "Your returned Haskell action stopped at an actor lifecycle failure. The typed failure is mounted as `sessionInput :: Maybe ActionFailure`; decide the next program explicitly.")
            (Just failure)
