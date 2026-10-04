{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# OPTIONS_GHC -Wno-simplifiable-class-constraints #-}

-- | One typed session owned by a supervised interactive agent application.
--
-- The optional prompt becomes that application's first User message. The
-- authoritative input remains a live Haskell value mounted in the persistent
-- workbench. Request settlement uses the separate 'Replies' effect.
module Tidepool.Agent.Session
  ( ActivationMetadata (..)
  , emptyActivationMetadata
  , attachAgent
  , requestSessionSited
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import Tidepool.Internal.RequestSite (RequestSite)

import Tidepool.Effects.Core (AgentSession (..), WorkerLifetime (..))

-- | Runtime activation data, separate from the caller's authored assignment.
data ActivationMetadata = ActivationMetadata
  { activationSiblings :: [(Text, Text, Text)]
  , activationRequestLifetime :: WorkerLifetime
  }

emptyActivationMetadata :: ActivationMetadata
emptyActivationMetadata = ActivationMetadata [] InvocationOwned

-- | Request this actor's Codex application without manufacturing a model turn.
attachAgent :: Member AgentSession effs => Maybe Text -> Eff effs ()
attachAgent initialUser = send (AgentAttachWith initialUser)

-- Engine-private request presentation. The request identity is runtime
-- authority; the site still carries GHC's input/result types.
{-# OPAQUE requestSessionSited #-}
requestSessionSited
  :: forall output input extra effs
   . Member AgentSession effs
  => RequestSite (input ': extra) output
  -> Int
  -> Maybe Text
  -> ActivationMetadata
  -> input
  -> Eff effs output
requestSessionSited site requestId initialUser metadata input =
  send (AgentSessionWith site input requestId initialUser (activationSiblings metadata))
