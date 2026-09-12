{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE TypeOperators #-}

-- | Stable actor addresses used by typed requests.
module Tidepool.Agent.Ref
  ( AgentRef (..)
  , AgentProtocol (..)
  , agentIdentity
  , agentBoundWorktree
  , internalAgentRef
  ) where

import Control.Monad.Freer (Eff)
import Prelude

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

agentIdentity :: AgentRef -> (Int, Int)
agentIdentity (AgentRef target _) = actorAddress target

agentBoundWorktree :: AgentRef -> Maybe WorktreeHandle
agentBoundWorktree (AgentRef _ tree) = tree

-- | Trusted workbench construction for its own exact incarnation.
internalAgentRef :: Int -> Int -> AgentRef
internalAgentRef actor incarnation =
  AgentRef (ActorRef actor incarnation (newExitCell ())) Nothing
