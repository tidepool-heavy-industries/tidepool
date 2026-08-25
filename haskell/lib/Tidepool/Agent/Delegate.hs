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

-- | The narrow delegation surface a recursive-companion
-- branch-node agent session compiles against.
--
-- @delegate@ is the ONLY verb this module exposes to a model-authored
-- block. Its row is @Member Delegate effs@ — never @Member Subagent effs@,
-- never @Member Worktree effs@ — so a block that tries to spell
-- @worktreeCreate@ or @send (SubagentSpawnAsync ...)@ directly fails with an
-- ordinary GHC "not a member of the type-level list" error, the same class
-- 'Tidepool.Agent.Spawn'/'Tidepool.Form' already produce for a row that
-- lacks their own effect.
--
-- 'runDelegate' is the Haskell-side interpreter (freer-simple
-- 'Control.Monad.Freer.reinterpret2' — the same free-monad-interposition
-- family 'Tidepool.Event.withHandler' is built from) that lowers
-- 'Delegate' onto the real 'Subagent' machinery
-- ("Tidepool.Agent.Spawn" speaks the same GADT). It never SENDS a raw
-- 'Tidepool.Effects.Worktree' constructor — worktree creation/binding
-- happens INSIDE the Rust spawn saga once 'Tidepool.Effects.SubagentSpawnAsync'
-- is dispatched (@tidepool-agent/CLAUDE.md@'s @SpawnSubstrate@ owns the
-- @WorktreeManager@ + @BindingTable@ directly) — it only needs
-- @Worktree@'s TYPE ('Tidepool.Effects.WorktreeSpec') to construct the
-- 'Tidepool.Effects.SpawnSpec' argument. @Worktree@ still has to ride in
-- the compiling ROW for real, though (not merely be nameable): @Subagent@'s
-- own auto-import (below) always pulls in @Tidepool.Agent.Spawn@, which
-- imports @Tidepool.Worktree (renderWorktreeError)@ — and GHC must
-- typecheck that WHOLE module to import anything from it, including its
-- own `M`-typed bindings (@worktreeBranch@, @worktreeHead@), which need
-- @Worktree@ genuinely present. 'runDelegate' re-adding it freshly via
-- 'reinterpret2' (rather than requiring a pre-existing @Member Worktree@)
-- is what keeps this from widening what the MODEL's own block can reach —
-- see below.
--
-- __Where this is reachable.__ Exactly where @Tidepool.Agent.Spawn@
-- already is — a row containing @Subagent@ — via the SAME
-- @extra_imports_for!@ row-gating (@tidepool-mcp/src/effect_defs.rs@).
--
-- __The wrap is the harness's job, not the model's.__ A branch-node
-- agent session's turn compiles the model's ENTIRE block as the argument to
-- 'runDelegate' (@Tidepool.Agent.Delegate.runDelegate $ do ...@), injected
-- by @tidepool-harness@ before the block reaches the extract compile — see
-- @EngineConfig.delegate_wrap@. That is what makes @Subagent@ AND
-- @Worktree@ genuinely unnameable to the model's own text, even though
-- both genuinely sit in the compiling row: the model's block-local row is
-- @Delegate ': effs@ (neither @Subagent@ nor @Worktree@ anywhere in
-- @effs@ itself), and only 'runDelegate''s OWN result type re-adds them,
-- freshly, on the outside — freer-simple's @reinterpret2@ signature
-- (@Eff (f ': effs) ~> Eff (g ': h ': effs)@) is what makes this a fresh
-- addition rather than something already reachable from inside the
-- argument. Wrapping a block that never calls 'delegate' is a semantic
-- no-op (@reinterpret2@'s @Val@ case and its pass-through of every other
-- effect leave an ordinary @finalize@-ending block untouched).
module Tidepool.Agent.Delegate
  ( Delegate
  , DelegateBrief (..)
  , DelegateResult (..)
  , DelegateError (..)
  , delegate
  , renderDelegateError
  , runDelegate
  ) where

import Prelude
import Data.Proxy (Proxy (..))
import GHC.Generics (Generic)

import Control.Monad.Freer (Eff, Member, reinterpret2, send)
import Tidepool.Effects
  ( CyclePayload (..)
  , DirtyPolicy (..)
  , Subagent (..)
  , SpawnError
  , SpawnOutcome (..)
  , Worktree
  , WorktreeSource (..)
  , WorktreeSpec (..)
  , spawnSpec
  )
import Tidepool.Aeson.FromJSON (FromJSON, fromJSON, resultToEither)
import Tidepool.Aeson.Schema (JsonSchema (..))
import Data.Text (Text)
import qualified Tidepool.Data.Text as T

-- | The narrow GADT: one verb, no result-shape escape hatch. Never a
-- 'RowArgs'/'EffectDecl' entry — it exists only between the model's
-- compile and 'runDelegate', never as a Rust registry row or a wire
-- effect.
data Delegate a where
  DelegateRequest :: DelegateBrief -> Delegate (Either DelegateError DelegateResult)

-- | What a branch-node agent session hands to a delegated subagent — delegation-
-- shaped (brief/instruction/expected result), never worktree-plumbing-
-- shaped. There is no field here a model could use to name an existing
-- worktree, pick a dirty-source policy, or otherwise reach the raw
-- 'Tidepool.Effects.WorktreeSpec' surface: 'runDelegate' always spawns a
-- FRESH worktree off the current repository's clean HEAD.
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

-- | The subagent's typed terminal payload. Single-constructor RECORD, not a
-- sum — 'Tidepool.Agent.Spawn''s documented constraint on any
-- @outputSchema@-derived result type (a sum renders a root @oneOf@, refused
-- at request validation).
data DelegateResult = DelegateResult
  { delegateSummary :: Text
  , delegateCaveats :: [Text]
  } deriving (Show, Eq, Generic, FromJSON, JsonSchema)

-- | Case-match the constructor to branch; 'renderDelegateError' is for
-- receipts/logs. Deliberately its OWN small type rather than a re-export of
-- 'Tidepool.Effects.SpawnError' — the delegation surface never hands the
-- model a value whose constructors mention 'Tidepool.Effects.WorktreeError'
-- or 'Tidepool.Effects.BackendFailure' by name.
data DelegateError
  = DelegateSpawnFailed Text
  | DelegateResultMalformed Text
  deriving (Show, Eq)

renderDelegateError :: DelegateError -> Text
renderDelegateError (DelegateSpawnFailed d) = "delegation failed to spawn: " <> d
renderDelegateError (DelegateResultMalformed d) = "delegation result malformed: " <> d

-- | The ONE verb a branch-node agent session may call. @Member Delegate effs@ is
-- the whole capability surface — never @Member Subagent effs@.
delegate :: Member Delegate effs => DelegateBrief -> Eff effs (Either DelegateError DelegateResult)
delegate = send . DelegateRequest

-- | Lower 'Delegate' onto the real 'Subagent' machinery. A fully
-- polymorphic, ordinary stdlib function — no per-turn code generation,
-- because it never needs to name the concrete row it runs in: 'Subagent'
-- and 'Worktree' are both freshly re-added on the OUTPUT side by
-- @reinterpret2@'s own signature, and @effs@ (whatever else the compiling
-- row carries — @AskUser@, @Fork@, @ReadState@, @Finalize T@) passes
-- through untouched. The handler never actually SENDS a @Worktree@
-- effect (see the module doc for why it doesn't need to) — @Worktree@
-- only needs to be nameable AS A TYPE here ('Tidepool.Effects.WorktreeSpec'),
-- and @reinterpret2@ requires no @Member Worktree@ obligation to discharge
-- one it never uses.
runDelegate :: forall effs a. Eff (Delegate ': effs) a -> Eff (Subagent ': Worktree ': effs) a
runDelegate = reinterpret2 handleDelegate
  where
    handleDelegate :: forall x. Delegate x -> Eff (Subagent ': Worktree ': effs) x
    handleDelegate (DelegateRequest brief) = do
      let expectedSuffix =
            if T.null (delegateExpected brief)
              then ""
              else "\n\nExpected result: " <> delegateExpected brief
          wspec =
            WorktreeSpec
              { specSource = SourceCurrentRepository
              , specLabel = delegateLabel brief
              , specDirtyPolicy = RequireClean
              }
          spec = spawnSpec wspec (delegateLabel brief) (delegateInstruction brief <> expectedSuffix)
          schema = jsonSchema (Proxy :: Proxy DelegateResult)
      spawned <- send (SubagentSpawnAsync spec schema)
      case spawned of
        Left err -> pure (Left (DelegateSpawnFailed (renderSpawnErrorLite err)))
        Right cyc -> do
          finished <- send (SubagentAwait cyc)
          case finished of
            Left err -> pure (Left (DelegateSpawnFailed (renderSpawnErrorLite err)))
            Right outcome -> pure (decodeDelegateOutcome outcome)

-- | A plain 'Show'-based rendering, deliberately not 'Tidepool.Agent.Spawn's
-- richer 'Tidepool.Agent.Spawn.renderSpawnError' — keeping this module free
-- of a 'Tidepool.Agent.Spawn'/'Tidepool.Worktree' dependency of its own
-- (it is already reached transitively, via 'Subagent''s own auto-import,
-- whenever a delegating row compiles). 'SpawnError' derives 'Show' (every
-- @errors@ block in this codebase does), so this is total.
renderSpawnErrorLite :: SpawnError -> Text
renderSpawnErrorLite = T.pack . show

decodeDelegateOutcome :: SpawnOutcome -> Either DelegateError DelegateResult
decodeDelegateOutcome outcome = case outcomePayload outcome of
  PayloadStructured v -> case resultToEither (fromJSON v) of
    Right value -> Right value
    Left detail -> Left (DelegateResultMalformed detail)
  PayloadUnstructured t -> Left (DelegateResultMalformed ("terminal message was not JSON: " <> t))
  PayloadAbsent -> Left (DelegateResultMalformed "no terminal message")
