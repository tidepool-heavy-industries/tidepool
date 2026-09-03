{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE PatternSynonyms #-}

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
import Tidepool.Agent.Action (AgentAction, runAgentAction)
import Tidepool.Agent.Session
  ( pattern ActionFailed
  , pattern InitialUser
  , pattern ManualReady
  , agentSession
  )
import Tidepool.Actors.Shoal

type RootEffects = ActorEffects

rootDriver :: Eff RootEffects a
rootDriver = loop InitialUser Nothing Nothing
  where
    loop activation initialUser interruption = do
      action <-
        ( agentSession activation initialUser interruption
            :: Eff RootEffects (AgentAction RootEffects ())
        )
      outcome <- runAgentAction action
      case outcome of
        Right () -> loop ManualReady Nothing Nothing
        Left failure ->
          loop
            ActionFailed
            Nothing
            (Just failure)
