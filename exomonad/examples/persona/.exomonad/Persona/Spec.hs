{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module Persona.Spec (agentSpec, helperSpec, PersonaEffects, HelperEffects) where
import GHC.Generics (Generic)
import Control.Monad.Freer (Members)
import Tidepool.Agent.Contract
import Tidepool.Effects.Core
  ( AskUser, Console, FsRead, FsWrite, Jev, Lookup, Reflect, Source, Web
  , ContextReadWrite, Actor, ActorContext, AgentLaunch, AgentInspection
  , AgentControl, Notifications, ResourceScopes, BoundWorktree )
import Tidepool.Agent.Reply (Replies)
import Tidepool.Agent.Watch (Watches)
import qualified Persona.Orientation as Orientation

-- Context edits settle before the next inference. Background work uses the
-- existing actor and request owners, with explicit lifetimes.
type PersonaEffects =
  '[AskUser, Console, FsRead, FsWrite, Jev, Lookup, Reflect, Source, Web
   , ContextReadWrite, Actor, ActorContext, AgentLaunch, AgentInspection
   , AgentControl, Notifications, ResourceScopes, BoundWorktree, Replies, Watches]
-- Helpers can explore, compose and curate their own context. Findings return
-- to the companion, which owns human-facing forms and durable memory edits.
type HelperEffects =
  '[Console, FsRead, Jev, Lookup, Reflect, Source, Web, ContextReadWrite
   , Actor, ActorContext, AgentLaunch, AgentInspection, AgentControl
   , Notifications, ResourceScopes, BoundWorktree, Replies, Watches]
data PersonaTools mode = PersonaTools
  { haskell :: mode :- Sync (HaskellCell PersonaEffects) }
  deriving (Generic)
agentSpec :: (Subset PersonaEffects (SyncEffects effects), Members '[FsRead, FsWrite] effects)
  => AgentSpec PersonaTools effects
agentSpec = defaultSpec { specTools = PersonaTools
  { haskell = haskellTool "Author tools, actors, context transformations, perspective forks, web exploration and interactive forms in Haskell. Bind full evidence before displaying selections. Persistent memory lives under .exomonad/state; running source is read through readSourceModule." }
  , initialContext = Just Orientation.startup }

data HelperTools mode = HelperTools
  { haskell :: mode :- Sync (HaskellCell HelperEffects) }
  deriving (Generic)
helperSpec :: Subset HelperEffects (SyncEffects effects) => AgentSpec HelperTools effects
helperSpec = defaultSpec { specTools = HelperTools
  { haskell = haskellTool "Compose an investigation in Haskell. Retain evidence, distinguish uncertainty, and return findings through the typed request. Curate your own context as useful; the companion owns durable memory edits and human-facing forms." } }
