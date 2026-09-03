{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# OPTIONS_GHC -Wno-simplifiable-class-constraints #-}

-- | One typed session owned by a supervised interactive agent application.
--
-- The optional prompt becomes that application's first User message. The
-- authoritative input remains a live Haskell value mounted in the persistent
-- workbench, and GHC checks the value supplied through 'complete' before the
-- installed actor program resumes.
module Tidepool.Agent.Session
  ( agentSession
  , SessionActivation (..)
  , agentSessionSited
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

import Tidepool.Effects.Core (AgentSession (..))

data SessionActivation
  = InitialUser
  | ActionCompleted
  | ActionFailed
  | ManualReady

activationCode :: SessionActivation -> Int
activationCode InitialUser = 0
activationCode ActionCompleted = 1
activationCode ActionFailed = 2
activationCode ManualReady = 3

{-# OPAQUE agentSession #-}
agentSession
  :: forall output input effs
   . Member AgentSession effs
  => SessionActivation
  -> Maybe Text
  -> input
  -> Eff effs output
agentSession activation initialUser input =
  agentSessionSited @output @input 0 activation initialUser input

-- Extractor substrate. The public fully-applied call is rewritten with the
-- site whose GHC-derived input/output types cross compiler metadata.
{-# OPAQUE agentSessionSited #-}
agentSessionSited
  :: forall output input effs
   . Member AgentSession effs
  => Int
  -> SessionActivation
  -> Maybe Text
  -> input
  -> Eff effs output
agentSessionSited site activation initialUser input =
  send (AgentSessionWith site input initialUser (activationCode activation))
