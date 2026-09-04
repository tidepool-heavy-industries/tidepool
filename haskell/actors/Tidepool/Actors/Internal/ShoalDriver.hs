{-# LANGUAGE DataKinds #-}
{-# LANGUAGE EmptyCase #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}

-- | Private permanent root application.
--
-- The authored model workbench is attached to this actor, but model-response
-- termination is not a Haskell effect. The private actor program simply owns
-- a mailbox receive continuation until its supervisor stops the root.
module Tidepool.Actors.Internal.ShoalDriver
  ( RootEffects
  , rootDriver
  , module Tidepool.Actors.Shoal
  ) where

import Control.Monad.Freer (Eff)
import Tidepool.Actor (serve)
import Tidepool.Agent.Session (attachAgent)
import Tidepool.Actors.Shoal
import Tidepool.Effects.Core (ActorLocal, AgentSession)

data RootProtocol result

type RootEffects = AgentSession ': ActorLocal RootProtocol ': ActorEffects

rootDriver :: Eff RootEffects a
rootDriver = do
  attachAgent Nothing
  serve @() @RootProtocol () (\() request -> case request of {})
