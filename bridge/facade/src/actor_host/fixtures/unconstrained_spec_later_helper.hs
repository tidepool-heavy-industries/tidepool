{-# LANGUAGE ConstraintKinds #-}
{-# LANGUAGE FlexibleContexts #-}
module AgentSpec (agentSpec, helper) where

import Control.Monad.Freer (Eff, Member)
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import Tidepool.Effects (Journal)

agentSpec :: AgentSpec NoTools effects
agentSpec = defaultSpec

type RequiresJournal effects = Member Journal effects

helper :: RequiresJournal effects => Eff effects ()
helper = pure ()
