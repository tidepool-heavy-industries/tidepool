{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}

-- | Independent idle subagents with explicit context, tools and ownership.
module Tidepool.Actors.Spawn
  ( SpawnContext (..)
  , ContextCheckpoint
  , checkpoint
  , releaseCheckpoint
  , Workspace (..)
  , WorkspaceHandle
  , WorktreeSeed
  , projectHead
  , currentCheckout
  , atRef
  , existingWorktree
  , currentWorkspace
  , SpawnOptions (..)
  , defaultSpawnOptions
  , SpawnError (..)
  , SpawnRetainedResources (..)
  , SpawnCleanup (..)
  , spawnSubagent
  , SpecReplacementError (..)
  , replaceSpec
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import Prelude
import Tidepool.Agent.Contract
  ( AgentSpec, HasInstalledAgentApi, KnownToolEffects, AsyncEffects, installSpec )
import Tidepool.Agent.Launch hiding (workspaceWire)
import qualified Tidepool.Agent.Launch as Launch
import Tidepool.Agent.Ref (AgentRef (..), agentIdentity)
import Tidepool.Effects.Row (KnownEffects (knownEffects), effectKeys)
import Tidepool.Effects.Core
  ( AgentLaunch (..), CheckpointRefusal, Model, ForkEffort
  , WorkerLifetime (..), SpawnContextWire (..), SpawnError (..)
  , SpawnRetainedResources (..), SpawnCleanup (..)
  , SpecReplacementError (..)
  )
import Tidepool.Internal.ActorRef (ActorRef (..))
import Tidepool.Internal.ExitCell (newExitCell)

-- | Runtime-issued exact provider and Haskell capture, never a source string.
newtype ContextCheckpoint = ContextCheckpoint Text
  deriving (Show, Eq)

data SpawnContext = ForkCtx ContextCheckpoint | FreshCtx Text
  deriving (Show, Eq)

checkpoint :: Member AgentLaunch effects => Text -> Eff effects (Either CheckpointRefusal ContextCheckpoint)
checkpoint name = fmap ContextCheckpoint <$> send (AgentLaunchCheckpointWith name)

releaseCheckpoint :: Member AgentLaunch effects => ContextCheckpoint -> Eff effects (Either CheckpointRefusal ())
releaseCheckpoint (ContextCheckpoint token) = send (AgentLaunchReleaseCheckpointWith token)

-- | The real typed spec is retained with all its compiled closure dependencies.
data SpawnOptions tools childEffects = SpawnOptions
  { spawnSpec :: AgentSpec tools childEffects
  , spawnModel :: Maybe Model
  , spawnEffort :: Maybe ForkEffort
  , spawnInstructions :: Maybe Text
  , spawnLabel :: Maybe Text
  , spawnLifetime :: WorkerLifetime
  , spawnLimits :: Maybe (Int, Int)
  }

defaultSpawnOptions :: AgentSpec tools childEffects -> SpawnOptions tools childEffects
defaultSpawnOptions spec = SpawnOptions spec Nothing Nothing Nothing Nothing ActorOwned Nothing

-- | Return only after workspace, tools and provider attachment are ready.
-- Fresh seed text creates context; it does not activate inference.
spawnSubagent
  :: forall tools childEffects effects.
     ( Member AgentLaunch effects, HasInstalledAgentApi tools childEffects
     , KnownEffects childEffects, KnownToolEffects childEffects, AsyncEffects childEffects )
  => SpawnContext -> Workspace -> SpawnOptions tools childEffects
  -> Eff effects (Either SpawnError AgentRef)
spawnSubagent context workspace options = do
  admitted <- send (AgentLaunchSpawnWith
    (contextWire context)
    (\_ -> installSpec @childEffects (spawnSpec options))
    (Launch.workspaceWire workspace)
    (effectKeys (knownEffects @childEffects))
    (spawnLabel options)
    (spawnModel options)
    (spawnEffort options)
    (spawnInstructions options)
    (spawnLifetime options)
    (spawnLimits options))
  pure $ fmap (\(actor, incarnation, tree) -> AgentRef (ActorRef actor incarnation (newExitCell ())) tree) admitted

contextWire :: SpawnContext -> SpawnContextWire
contextWire (ForkCtx (ContextCheckpoint token)) = CapturedSpawn token
contextWire (FreshCtx prompt) = FreshSpawn prompt

-- | Replace an installation atomically only when its tool surface is equal.
replaceSpec
  :: forall tools childEffects effects.
     ( Member AgentLaunch effects, HasInstalledAgentApi tools childEffects
     , KnownEffects childEffects, KnownToolEffects childEffects, AsyncEffects childEffects )
  => AgentRef -> AgentSpec tools childEffects
  -> Eff effects (Either SpecReplacementError ())
replaceSpec agent spec = send (AgentLaunchReplaceSpecWith
  (agentIdentity agent)
  (\_ -> installSpec @childEffects spec)
  (effectKeys (knownEffects @childEffects)))
