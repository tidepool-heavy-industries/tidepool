{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | Stable actor addresses used by typed requests.
module Tidepool.Agent.Ref
  ( AgentRef (..)
  , AgentProtocol (..)
  , agentIdentity
  , agentAddressText
  , agentBoundWorktree
  , internalAgentRef
  ) where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import qualified Data.Text as Text
import Prelude

import Tidepool.Inspection.Display (Display (..), opaqueHandle)

import Tidepool.Internal.ActorRef (ActorRef (..), actorAddress)
import Tidepool.Internal.ExitCell (newExitCell)
import Tidepool.Effects.Core
  ( ActorLocal, AgentTools, AgentSession, Actor, FsRead, Worktree
  , Notifications, Console, Sleep, WorktreeHandle
  )

data AgentProtocol result where
  RunRequest
    :: Eff
      '[ ActorLocal AgentProtocol, AgentTools, AgentSession, Actor, FsRead
       , Worktree, Notifications, Console, Sleep
       ] ()
    -> AgentProtocol ()

data AgentRef = AgentRef
  (ActorRef AgentProtocol ())
  (Maybe WorktreeHandle)

instance Show AgentRef where
  show agent = "AgentRef " <> show (agentIdentity agent)

-- | The workbench shows an agent the way status and notices do: @<agent 2\@1>@.
instance Display AgentRef where
  displayTree agent = opaqueHandle ("agent " <> agentAddressText agent)

agentIdentity :: AgentRef -> (Int, Int)
agentIdentity (AgentRef target _) = actorAddress target

-- | @2\@1@: actor id, then incarnation, as every model-facing surface spells it.
agentAddressText :: AgentRef -> Text
agentAddressText agent =
  let (actor, incarnation) = agentIdentity agent
   in Text.pack (show actor) <> "@" <> Text.pack (show incarnation)

agentBoundWorktree :: AgentRef -> Maybe WorktreeHandle
agentBoundWorktree (AgentRef _ tree) = tree

-- | Trusted workbench construction for its own exact incarnation. The exit
-- cell is a permanently pending placeholder; use this reference for its
-- address, not for observing the actor's exit.
internalAgentRef :: Int -> Int -> AgentRef
internalAgentRef actor incarnation =
  AgentRef (ActorRef actor incarnation (newExitCell ())) Nothing
