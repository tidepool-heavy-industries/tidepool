{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE PatternSynonyms #-}
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
  , SessionActivation
  , pattern InitialUser
  , pattern ActionCompleted
  , pattern ActionFailed
  , pattern ManualReady
  , agentSessionSited
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

import Tidepool.Effects.Core (AgentSession (..))

-- Keep the public vocabulary closed while giving the extractor an unboxed
-- representation at the generated effect boundary.
newtype SessionActivation = SessionActivation Int

pattern InitialUser :: SessionActivation
pattern InitialUser = SessionActivation 0

pattern ActionCompleted :: SessionActivation
pattern ActionCompleted = SessionActivation 1

pattern ActionFailed :: SessionActivation
pattern ActionFailed = SessionActivation 2

pattern ManualReady :: SessionActivation
pattern ManualReady = SessionActivation 3

{-# COMPLETE InitialUser, ActionCompleted, ActionFailed, ManualReady #-}

activationCode :: SessionActivation -> Int
activationCode (SessionActivation code) = code

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
