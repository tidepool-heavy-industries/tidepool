{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# OPTIONS_GHC -Wno-simplifiable-class-constraints #-}

-- | One typed session owned by a supervised interactive agent application.
--
-- The optional prompt becomes that application's first User message. The
-- authoritative input remains a live Haskell value mounted in the persistent
-- workbench. Request settlement uses the separate 'Replies' effect.
module Tidepool.Agent.Session
  ( attachAgent
  , requestSessionSited
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

import Tidepool.Effects.Core (AgentSession (..))

-- | Request this actor's Codex application without manufacturing a model turn.
attachAgent :: Member AgentSession effs => Maybe Text -> Eff effs ()
attachAgent initialUser = send (AgentAttachWith initialUser)

-- Engine-private request presentation. The request identity is runtime
-- authority; the site still carries GHC's input/result types.
{-# OPAQUE requestSessionSited #-}
requestSessionSited
  :: forall output input effs
   . Member AgentSession effs
  => Int
  -> Int
  -> Maybe Text
  -> input
  -> Eff effs output
requestSessionSited site requestId initialUser input =
  send (AgentSessionWith site input requestId initialUser)
