{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
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
  , SpawnOptions
  , spawnModel
  , spawnReasoning
  , spawnInstructions
  , spawnLabel
  , spawnLifetime
  , spawnLimits
  , ValidatedAgentSpec
  , AgentSpecValidationReport (..)
  , SpecValidationError (..)
  , SpecValidationReceipt (..)
  , SpecProvenance (..), SpecProviderValidation (..)
  , validateAgentSpec
  , validatedAgentSpecReport
  , defaultValidatedSpawnOptions
  , SpawnLimits (..)
  , DescendantDepth
  , ActiveDescendants
  , SpawnLimitError (..)
  , descendantDepth
  , activeDescendants
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
import Tidepool.Aeson (Value, ToJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import GHC.Generics (Generic)
import Numeric.Natural (Natural)
import Prelude
import Tidepool.Agent.Contract
  ( AgentSpec, HasInstalledAgentApi, KnownToolEffects, AsyncEffects, installSpec
  , PreparedAgentSpec, prepareAgentSpec, preparedAgentSpecMetadata, installPreparedSpec
  , renderToolCompileError )
import Tidepool.Agent.Launch hiding (workspaceWire)
import qualified Tidepool.Agent.Launch as Launch
import Tidepool.Agent.Ref.Internal (AgentRef (..), agentIdentity)
import Tidepool.Actors.Internal.Agent (installRequestReceiver)
import Tidepool.Effects.Row (KnownEffects (knownEffects), effectKeys)
import Tidepool.Effects.Core
  ( AgentLaunch (..), CheckpointRefusal, Model, Reasoning
  , WorkerLifetime (..), SpawnContextWire (..)
  , SpawnCleanup (..)
  , SpecReplacementError (..), SpecValidationError (..), SpecValidationReceipt (..), SpecProvenance (..), SpecProviderValidation (..)
  )
import qualified Tidepool.Effects.Core as Core
import Tidepool.Internal.ActorRef (ActorRef (..))

-- | Runtime-issued exact provider and Haskell capture, never a source string.
newtype ContextCheckpoint = ContextCheckpoint Text
  deriving (Show, Eq)

data SpawnContext = ForkCtx ContextCheckpoint | FreshCtx Text
  deriving (Show, Eq)

checkpoint :: Member AgentLaunch effects => Text -> Eff effects (Either CheckpointRefusal ContextCheckpoint)
checkpoint name = fmap ContextCheckpoint <$> send (AgentLaunchCheckpointWith name)

releaseCheckpoint :: Member AgentLaunch effects => ContextCheckpoint -> Eff effects (Either CheckpointRefusal ())
releaseCheckpoint (ContextCheckpoint token) = send (AgentLaunchReleaseCheckpointWith token)

-- | A partial admission retains usable runtime-issued handles for cleanup.
data SpawnRetainedResources
  = SpawnRetainedWorkspace Core.WorktreeHandle
  | SpawnRetainedActor AgentRef
  deriving (Show)

data SpawnError
  = SpawnInvalidSpec SpecValidationError
  | SpawnRefused Text
  | SpawnPartialFailure SpawnRetainedResources SpawnCleanup Text
  deriving (Show)

-- | Maximum further spawn depth and sponsored active descendants of the child.
-- Zero forbids further descendants. These caps only attenuate parent limits.
-- Active descendants include pending admissions throughout its spawn ancestry.
data SpawnLimits = SpawnLimits
  { maximumDescendantDepth :: DescendantDepth
  , maximumActiveDescendants :: ActiveDescendants
  }
  deriving (Show, Eq)

newtype DescendantDepth = DescendantDepth Int
  deriving (Show, Eq, Ord)

newtype ActiveDescendants = ActiveDescendants Int
  deriving (Show, Eq, Ord)

data SpawnLimitError
  = DescendantDepthTooLarge Natural
  | ActiveDescendantsTooLarge Natural
  deriving (Show, Eq)

-- | Accepted quantities match the runtime's inclusive 0..65535 range.
descendantDepth :: Natural -> Either SpawnLimitError DescendantDepth
descendantDepth value
  | value > 65535 = Left (DescendantDepthTooLarge value)
  | otherwise = Right (DescendantDepth (fromIntegral value))

activeDescendants :: Natural -> Either SpawnLimitError ActiveDescendants
activeDescendants value
  | value > 65535 = Left (ActiveDescendantsTooLarge value)
  | otherwise = Right (ActiveDescendants (fromIntegral value))

limitsWire :: SpawnLimits -> (Int, Int)
limitsWire (SpawnLimits (DescendantDepth depth) (ActiveDescendants active)) = (depth, active)

-- | The real typed spec is retained with all its compiled closure dependencies.
data SpawnOptions tools childEffects = SpawnOptions
  { selectedSpawnSpec :: SpawnSpec tools childEffects
  , spawnModel :: Maybe Model
  , spawnReasoning :: Maybe Reasoning
  , spawnInstructions :: Maybe Text
  , spawnLabel :: Maybe Text
  , spawnLifetime :: WorkerLifetime
  -- Nothing inherits the parent's attenuated limits, including unbounded width.
  , spawnLimits :: Maybe SpawnLimits
  }

defaultSpawnOptions :: AgentSpec tools childEffects -> SpawnOptions tools childEffects
defaultSpawnOptions spec = SpawnOptions (RawSpawnSpec spec) Nothing Nothing Nothing Nothing ActorOwned Nothing

data SpawnSpec tools effects
  = RawSpawnSpec (AgentSpec tools effects)
  | ValidatedSpawnSpec (ValidatedAgentSpec tools effects)

data ValidatedAgentSpec tools effects = ValidatedAgentSpec (PreparedAgentSpec tools effects) SpecValidationReceipt

data AgentSpecValidationReport = AgentSpecValidationReport
  { validationInstallation :: Value
  , validationReceipt :: SpecValidationReceipt
  } deriving (Show, Generic)

instance ToJSON AgentSpecValidationReport
instance JsonSchema AgentSpecValidationReport

validatedAgentSpecReport :: ValidatedAgentSpec tools effects -> AgentSpecValidationReport
validatedAgentSpecReport (ValidatedAgentSpec prepared receipt) =
  AgentSpecValidationReport (preparedAgentSpecMetadata prepared) receipt

-- | Reuse the exact prepared value. Spawn still rechecks current admission;
-- editing draft source cannot substitute another spec into this value.
defaultValidatedSpawnOptions :: ValidatedAgentSpec tools effects -> SpawnOptions tools effects
defaultValidatedSpawnOptions validated = SpawnOptions (ValidatedSpawnSpec validated) Nothing Nothing Nothing Nothing ActorOwned Nothing

-- | Inspect this concrete value's declarations on the current host without
-- allocating a child or running authored handlers, startup or after-tool code.
-- Acceptance observes current interpreter support, not future launch success.
-- A report is evidence, not an authority token or proof of handler behavior.
validateAgentSpec
  :: forall tools childEffects effects.
     (Member AgentLaunch effects, HasInstalledAgentApi tools childEffects,
      KnownEffects childEffects, KnownToolEffects childEffects, AsyncEffects childEffects)
  => AgentSpec tools childEffects -> Eff effects (Either SpecValidationError (ValidatedAgentSpec tools childEffects))
validateAgentSpec spec = case prepareAgentSpec spec of
  Left failure -> pure (Left (SpecPreparationFailed (renderToolCompileError failure)))
  Right prepared -> validatePreparedSpec prepared

validatePreparedSpec
  :: forall tools childEffects effects. (Member AgentLaunch effects, KnownEffects childEffects)
  => PreparedAgentSpec tools childEffects -> Eff effects (Either SpecValidationError (ValidatedAgentSpec tools childEffects))
validatePreparedSpec prepared = do
  result <- send (AgentLaunchValidateSpecWith (preparedAgentSpecMetadata prepared) (effectKeys (knownEffects @childEffects)))
  pure (ValidatedAgentSpec prepared <$> result)

-- | Return only after workspace, tools and provider attachment are ready.
-- Fresh seed text creates context; it does not activate inference.
spawnSubagent
  :: forall tools childEffects effects.
     ( Member AgentLaunch effects, HasInstalledAgentApi tools childEffects
     , KnownEffects childEffects, KnownToolEffects childEffects, AsyncEffects childEffects )
  => SpawnContext -> Workspace -> SpawnOptions tools childEffects
  -> Eff effects (Either SpawnError AgentRef)
spawnSubagent context workspace options = do
  validation <- case selectedSpawnSpec options of
    RawSpawnSpec spec -> validateAgentSpec spec
    ValidatedSpawnSpec validated -> pure (Right validated)
  case validation of
    Left failure -> pure (Left (SpawnInvalidSpec failure))
    Right (ValidatedAgentSpec prepared _) -> launch prepared
  where
    launch prepared = do
     admitted <- send (AgentLaunchSpawnWith
      (contextWire context)
      (\_ -> do
        installPreparedSpec prepared
        installRequestReceiver)
      (Launch.workspaceWire workspace)
      (effectKeys (knownEffects @childEffects))
      (spawnLabel options)
      (spawnModel options)
      (spawnReasoning options)
      (spawnInstructions options)
      (spawnLifetime options)
      (limitsWire <$> spawnLimits options)
      (preparedAgentSpecMetadata prepared))
     pure $ case admitted of
      Left failure -> Left (spawnError failure)
      Right (actor, incarnation, tree) -> Right (admittedAgent actor incarnation tree)

-- These identities come only from the interpreter's admission receipt.
admittedAgent :: Int -> Int -> Maybe Core.WorktreeHandle -> AgentRef
admittedAgent actor incarnation tree =
  AgentRef (ActorRef actor incarnation) tree

spawnError :: Core.SpawnErrorWire -> SpawnError
spawnError (Core.SpawnRefused cause) = SpawnRefused cause
spawnError (Core.SpawnPartialFailure retained cleanup cause) =
  SpawnPartialFailure (retainedResources retained) cleanup cause

retainedResources :: Core.SpawnRetainedResourcesWire -> SpawnRetainedResources
retainedResources (Core.SpawnRetainedWorkspace workspace) = SpawnRetainedWorkspace workspace
retainedResources (Core.SpawnRetainedActor (actor, incarnation) workspace) =
  SpawnRetainedActor (admittedAgent actor incarnation workspace)

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
