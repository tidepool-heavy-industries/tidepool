{-# LANGUAGE ConstraintKinds #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
{-# OPTIONS_GHC -Wno-simplifiable-class-constraints #-}

-- | Synchronous transcript curation and explicit captured-context delegation.
-- Actor readiness and typed request admission are independent operations.
module ContextWorkflow where

import Control.Lens (traversed, (^..))
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import qualified Jev.Operators as J
import Jev.Tidepool ()

import Tidepool.Actors.Exomonad
import Tidepool.Agent.Context
import qualified Tidepool.Agent.Context as C
import Tidepool.Agent.Contract (AgentSpec, HasInstalledAgentApi, KnownToolEffects, AsyncEffects)
import Tidepool.Effects.Core (ContextReadWrite, Jev)

sharedFinding :: Text
sharedFinding = "The cache key must include the selected transcript prefix."

-- Keep the successful conclusion while replacing a verbose completed tool
-- result. The returned suffix is copied from that exact source; the marker is
-- ordinary authored text and has no runtime meaning.
trimBuildOutput :: Text -> Text
trimBuildOutput original
  | "[Trimmed:" `T.isPrefixOf` original = original
  | T.length original > 800 && "BUILD SUCCEEDED" `T.isInfixOf` original =
      C.trimText "repetitive build output; final 800 characters retained"
        (T.takeEnd 800 original)
  | otherwise = original

-- Build changes only for eligible tool-result fields, retaining each exact
-- selector and issued content version. No effect occurs while building a patch.
trimToolResults :: Context -> [ContextChange]
trimToolResults current =
  [ replaceText target updated
  | target <- textTargets current
  , targetSelector target == ToolResultText
  , let updated = trimBuildOutput (targetText target)
  , updated /= targetText target
  ]

trimEveryLongVisibleText :: Int -> Text -> Context -> ContextPatch
trimEveryLongVisibleText limit reason current = contextPatch
  [ replaceText target (C.trimText reason (T.takeEnd limit original))
  | target <- textTargets current
  , let original = targetText target
  , T.length original > limit
  ]

-- One synchronous cell stages one batch. A staged handle identifies pending
-- work; inspect it in a later cell for the terminal receipt.
curateChild :: Member ContextReadWrite effects => Eff effects ContextStageResult
curateChild = do
  current <- getContext
  staged <- stageContext (contextPatch
    (trimToolResults current ++ [appendNote ("Child finding: " <> sharedFinding) []]))
  case staged of
    ContextStaged _ -> C.setNextEffort C.High
    ContextStageRefused _ -> pure ()
  pure staged

-- Model and effort are staged by the same synchronous effect. Use a model
-- change only for a context/model combination the runtime has qualified;
-- incompatible opaque history must surface as an explicit refusal.
stageNextModelAndEffort :: Member ContextReadWrite effects => Text -> Eff effects ()
stageNextModelAndEffort alias = do
  setNextModel alias
  C.setNextEffort C.High

-- Call after the editing cell has succeeded and its transaction is committed.
-- The caller supplies the actual installed tool definition. Each refusal keeps
-- its concrete spawned actor, when present, available for explicit recovery.
data ContextDelegation
  = ContextSpawnRefused SpawnError
  | ContextRequestRefused AgentRef RequestError
  | ContextRequested AgentRef (Request Text)
  deriving (Show)

delegateCuratedContext
  :: ( Member AgentLaunch effects, Member Replies effects
     , KnownEffects childEffects, KnownToolEffects childEffects, AsyncEffects childEffects
     , HasInstalledAgentApi tools childEffects
     )
  => AgentSpec tools childEffects
  -> Eff effects (Either CheckpointRefusal ([ContextDelegation], Either CheckpointRefusal ()))
delegateCuratedContext actualSpec = do
  captured <- checkpoint "curated context"
  case captured of
    Left issue -> pure (Left issue)
    Right context -> do
      idle <- mapM (\name -> spawnSubagent (ForkCtx context) SameDir
        ((defaultSpawnOptions actualSpec) { spawnLabel = Just name }))
        ["review-api", "review-tests"]
      admitted <- mapM activate (zip idle
        ["Review the API against: " <> sharedFinding, "Review the tests against: " <> sharedFinding])
      released <- releaseCheckpoint context
      pure (Right (admitted, released))
  where
    activate (Left issue, _) = pure (ContextSpawnRefused issue)
    activate (Right actor, input) = do
      admitted <- request @Text actor input defaultRequestOptions
      pure (either (ContextRequestRefused actor) (ContextRequested actor) admitted)

-- Preview one version-bound batch before staging it. This targets only full
-- eligible bodies and appends an assistant note; it keeps native structure.
inspectAndCurate
  :: Member ContextReadWrite effects
  => Eff effects (Either [ContextProblem] (ContextPreview, ContextStageResult))
inspectAndCurate = do
  before <- getContext
  let patch = contextPatch
        (trimToolResults before ++ [appendNote ("Child finding: " <> sharedFinding) []])
  case previewContext before patch of
    Left problems -> pure (Left problems)
    Right preview -> do
      staged <- stageContext patch
      pure (Right (preview, staged))

-- Convert whole removable exchanges separately from field edits: removing a
-- unit and editing one of its fields in the same batch is an invalid overlap.
completedNotes :: Context -> Either [ContextProblem] ContextPatch
completedNotes before = toNotes
  [ reference
  | block <- before ^.. contextBlocks . traversed
  , NativeProvenance reference <- [blockProvenance block]
  , blockKind block == NativeEvidence CompletedExchange
  , reference `elem` map unitReference (unitTargets before)
  ] before

-- Jev judges each packet containing the original slice once. The returned
-- values come from the original rows, never from model-written replacements.
selectOriginalSlices
  :: Member Jev effects
  => [(ContextReference, Text)]
  -> Eff effects [(ContextReference, Text)]
selectOriginalSlices originalSlices = do
  let indexed = zip [0 :: Int ..] originalSlices
      packet = #selected J.:= J.each (\(index, _, _) -> T.pack (show index))
        (\(index, _, original) -> #keep J.:= J.noul
          ("Keep this exact original slice at index " <> T.pack (show index) <> "?\n" <> original))
        [(index, reference, original) | (index, (reference, original)) <- indexed]
  result <- J.ask (J.state (#purpose J.:= ("Select relevant source slices." :: Text))) packet
  case result of
    Left _ -> pure originalSlices
    Right answer ->
      pure
        [ (reference, original)
        | ((_, reference, original), decision) <- answer.selected
        , J.holds J.careful decision.keep
        ]
