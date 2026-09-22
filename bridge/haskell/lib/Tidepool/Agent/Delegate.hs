{-# LANGUAGE AllowAmbiguousTypes #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}

-- | A narrow, typed delegation surface for agent-authored orchestration.
--
-- Agent-authored code describes a delegate request and its Haskell result
-- type.  The interpreter owns workspace allocation, subagent spawning, and
-- decoding the result.  Its row is @Member Delegate effs@, never
-- @Member Subagent effs@ or @Member Worktree effs@, so those substrate
-- capabilities remain outside the authored block.
--
-- 'runDelegate' lowers this effect onto the existing 'Subagent' and
-- 'Worktree' machinery.  A fresh delegate can start from the current node or
-- a retained candidate; a revision can re-enter that candidate.  The result
-- contains both the model-produced value and the workspace identities the
-- interpreter observed.
--
-- The harness wraps the entire model-authored block in 'runDelegate'.  That
-- gives the block a local @Delegate ': effs@ row while adding @Subagent@ and
-- @Worktree@ only outside it via 'reinterpret2'.
module Tidepool.Agent.Delegate
  ( Delegate
  , DelegateBrief (..)
  , DelegateResult (..)
  , DelegateRun (..)
  , DelegateError (..)
  , delegate
  , delegateTyped
  , delegateTypedFrom
  , delegateTypedIn
  , renderDelegateError
  , runDelegate
  ) where

import Prelude
import Data.Proxy (Proxy (..))
import GHC.Generics (Generic)

import Control.Monad.Freer (Eff, Member, reinterpret2, send)
import Tidepool.Effects.Core
  ( CyclePayload (..)
  , DirtyPolicy (..)
  , GitOid
  , Subagent (..)
  , SpawnError
  , SpawnOutcome (..)
  , WorkerRun (..)
  , Worktree (..)
  , WorktreeError
  , WorktreeHandle (..)
  , WorktreeReceipt (..)
  , WorktreeSource (..)
  , WorktreeSpec (..)
  , spawnSpecIn
  , spawnSpec
  , worktreeId
  )
import Tidepool.Aeson.FromJSON (FromJSON, fromJSON, resultToEither)
import Tidepool.Aeson.Schema (JsonSchema (..))
import Data.Text (Text)
import qualified Tidepool.Data.Text as T

-- | The narrow GADT carrying the caller's requested result type. Never a
-- 'RowArgs'/'EffectDecl' entry — it exists only between the model's
-- compile and 'runDelegate', never as a Rust registry row or a wire
-- effect.
data Delegate a where
  DelegateRequest
    :: (FromJSON result, JsonSchema result)
    => Proxy result
    -> DelegateWorkspace
    -> DelegateBrief
    -> Delegate (Either DelegateError (DelegateRun result))

-- | Where the trusted interpreter should place one delegated agent.  Kept
-- private: model-authored code supplies a semantic brief, not a worktree
-- allocation policy.
data DelegateWorkspace
  = FreshFromCurrent
  | FreshFromWorktree WorktreeHandle
  | ExistingWorktree WorktreeHandle

-- | What an owner hands to a delegated subagent: a semantic brief, with no
-- worktree allocation policy exposed in the value.
data DelegateBrief = DelegateBrief
  { delegateLabel :: Text
    -- ^ Short slug (rendered into the subagent's worktree label and
    -- receipts).
  , delegateInstruction :: Text
    -- ^ The task, in prose.
  , delegateExpected :: Text
    -- ^ What a good result looks like, in prose — steers the subagent's own
    -- terminal summary. May be empty.
  } deriving (Show, Eq)

-- | The original convenience result.  'delegateTyped' also accepts any
-- caller-chosen @FromJSON@/@JsonSchema@ result, including sum types.
data DelegateResult = DelegateResult
  { delegateSummary :: Text
  , delegateCaveats :: [Text]
  } deriving (Show, Eq, Generic, FromJSON, JsonSchema)

-- | One completed delegate call: the model-produced value plus the workspace
-- facts observed by the interpreter.  The value is typed by the caller's
-- requested result; the handle and git identities are never part of the
-- model's JSON schema.
data DelegateRun result = DelegateRun
  { delegateValue :: result
  , delegateWorktree :: WorktreeHandle
  , delegateBase :: GitOid
  , delegateHead :: GitOid
  }
  deriving (Show)

-- | Case-match the constructor to branch; 'renderDelegateError' is for
-- receipts/logs. Deliberately its OWN small type rather than a re-export of
-- 'Tidepool.Effects.SpawnError' — the delegation surface never hands the
-- model a value whose constructors mention 'Tidepool.Effects.WorktreeError'
-- or 'Tidepool.Effects.BackendFailure' by name.
data DelegateError
  = DelegateSpawnFailed Text
  | DelegateResultMalformed Text
  | DelegateWorkspaceFailed Text
  deriving (Show, Eq)

renderDelegateError :: DelegateError -> Text
renderDelegateError (DelegateSpawnFailed d) = "delegation failed to spawn: " <> d
renderDelegateError (DelegateResultMalformed d) = "delegation result malformed: " <> d
renderDelegateError (DelegateWorkspaceFailed d) = "delegation workspace could not be read: " <> d

-- | Backwards-compatible convenience request using 'DelegateResult'.
delegate :: Member Delegate effs => DelegateBrief -> Eff effs (Either DelegateError DelegateResult)
delegate brief = fmap (fmap delegateValue) (delegateTyped brief)

-- | Ask for an arbitrary typed result in a fresh worktree rooted at the
-- current node's repository.  The interpreter derives the schema from the
-- Haskell result type.
delegateTyped
  :: forall result effs.
     (Member Delegate effs, FromJSON result, JsonSchema result)
  => DelegateBrief
  -> Eff effs (Either DelegateError (DelegateRun result))
delegateTyped = send . DelegateRequest (Proxy :: Proxy result) FreshFromCurrent

-- | Run a read/review delegate in a fresh isolated worktree rooted at an
-- existing candidate's current HEAD.
delegateTypedFrom
  :: forall result effs.
     (Member Delegate effs, FromJSON result, JsonSchema result)
  => WorktreeHandle
  -> DelegateBrief
  -> Eff effs (Either DelegateError (DelegateRun result))
delegateTypedFrom tree = send . DelegateRequest (Proxy :: Proxy result) (FreshFromWorktree tree)

-- | Run a revision delegate in an existing retained worktree.  The spawn
-- substrate enforces that the worktree is currently unbound.
delegateTypedIn
  :: forall result effs.
     (Member Delegate effs, FromJSON result, JsonSchema result)
  => WorktreeHandle
  -> DelegateBrief
  -> Eff effs (Either DelegateError (DelegateRun result))
delegateTypedIn tree = send . DelegateRequest (Proxy :: Proxy result) (ExistingWorktree tree)

-- | Lower 'Delegate' onto the real 'Subagent' and 'Worktree' machinery while
-- passing every other effect through untouched.
runDelegate :: forall effs a. Eff (Delegate ': effs) a -> Eff (Subagent ': Worktree ': effs) a
runDelegate = reinterpret2 handleDelegate
  where
    handleDelegate :: forall x. Delegate x -> Eff (Subagent ': Worktree ': effs) x
    handleDelegate (DelegateRequest resultProxy workspace brief) = do
      let expectedSuffix =
            if T.null (delegateExpected brief)
              then ""
              else "\n\nExpected result: " <> delegateExpected brief
          task = delegateInstruction brief <> expectedSuffix
          spec = case workspace of
            FreshFromCurrent ->
              spawnSpec
                WorktreeSpec
                  { specSource = SourceCurrentRepository
                  , specLabel = delegateLabel brief
                  , specDirtyPolicy = RequireClean
                  }
                (delegateLabel brief)
                task
            FreshFromWorktree tree ->
              spawnSpec
                WorktreeSpec
                  { specSource = SourceWorktree (worktreeId tree)
                  , specLabel = delegateLabel brief
                  , specDirtyPolicy = RequireClean
                  }
                (delegateLabel brief)
                task
            ExistingWorktree tree ->
              spawnSpecIn (worktreeId tree) (delegateLabel brief) task
          schema = jsonSchema resultProxy
      spawned <- send (SubagentSpawnAsync spec schema)
      case spawned of
        Left err -> pure (Left (DelegateSpawnFailed (renderSpawnErrorLite err)))
        Right cyc -> do
          finished <- send (SubagentAwait cyc)
          case finished of
            Left err -> pure (Left (DelegateSpawnFailed (renderSpawnErrorLite err)))
            Right outcome -> finishDelegate outcome

    finishDelegate
      :: forall result.
         FromJSON result
      => SpawnOutcome
      -> Eff (Subagent ': Worktree ': effs) (Either DelegateError (DelegateRun result))
    finishDelegate outcome = case decodeDelegateOutcome outcome of
      Left err -> pure (Left err)
      Right value -> do
        let tree = runWorktree (outcomeRun outcome)
            base = sourceHead (handleReceipt tree)
        current <- send (WorktreeHeadOf (worktreeId tree))
        pure $ case current of
          Left err -> Left (DelegateWorkspaceFailed (renderWorktreeFailure err))
          Right headOid -> Right DelegateRun
            { delegateValue = value
            , delegateWorktree = tree
            , delegateBase = base
            , delegateHead = headOid
            }

-- | A plain 'Show'-based rendering, deliberately not 'Tidepool.Agent.Spawn's
-- richer 'Tidepool.Agent.Spawn.renderSpawnError' — keeping this module free
-- of a 'Tidepool.Agent.Spawn'/'Tidepool.Worktree' dependency of its own
-- (it is already reached transitively, via 'Subagent''s own auto-import,
-- whenever a delegating row compiles). 'SpawnError' derives 'Show' (every
-- @errors@ block in this codebase does), so this is total.
renderSpawnErrorLite :: SpawnError -> Text
renderSpawnErrorLite = T.pack . show

decodeDelegateOutcome :: FromJSON result => SpawnOutcome -> Either DelegateError result
decodeDelegateOutcome outcome = case outcomePayload outcome of
  PayloadStructured v -> case resultToEither (fromJSON v) of
    Right value -> Right value
    Left detail -> Left (DelegateResultMalformed detail)
  PayloadUnstructured t -> Left (DelegateResultMalformed ("terminal message was not JSON: " <> t))
  PayloadAbsent -> Left (DelegateResultMalformed "no terminal message")

renderWorktreeFailure :: WorktreeError -> Text
renderWorktreeFailure = T.pack . show
