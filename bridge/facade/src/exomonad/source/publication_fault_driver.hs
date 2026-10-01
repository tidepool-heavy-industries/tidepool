{-# LANGUAGE DataKinds #-}
{-# LANGUAGE EmptyCase #-}
{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
module PublicationFaultDriver (FaultEffects, faultDriver) where

import Control.Monad.Freer (Eff)
import Tidepool.Actor (serve)
import Tidepool.Agent.Session (attachAgent)
import Tidepool.Effects.Core (Actor, ActorLocal, AgentSession, AgentTools)

data FaultProtocol result

type FaultEffects = '[AgentSession, ActorLocal FaultProtocol, AgentTools, Actor]

faultDriver :: Eff FaultEffects ()
faultDriver = do
  attachAgent Nothing
  serve @() @FaultProtocol () (\() request -> case request of {})
