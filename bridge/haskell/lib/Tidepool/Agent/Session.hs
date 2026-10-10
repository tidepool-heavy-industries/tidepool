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
  ( attachAgent
  , publishResponse
  , publishProgressResponse
  , requestSessionSited
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import Tidepool.Internal.RequestSite (RequestSite)

import Tidepool.Effects.Core (AgentSession (..))
import Tidepool.Agent.Reply.Internal (ResponseResult)

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
  -> input
  -> Eff effs output
requestSessionSited site requestId initialUser input =
  send (AgentSessionWith site input requestId initialUser [])

-- | Publish the fully assembled result under the original protected request site.
{-# OPAQUE publishResponse #-}
publishResponse
  :: forall result input effs. Member AgentSession effs
  => Int -> RequestSite '[input, ResponseResult result] result -> ResponseResult result -> Eff effs ()
publishResponse requestId site response = send (AgentSessionPublishResponseWith requestId site response)

{-# OPAQUE publishProgressResponse #-}
publishProgressResponse
  :: forall result input progress effs. Member AgentSession effs
  => Int -> RequestSite '[input, progress, ResponseResult result] result -> ResponseResult result -> Eff effs ()
publishProgressResponse requestId site response = send (AgentSessionPublishProgressResponseWith requestId site response)
