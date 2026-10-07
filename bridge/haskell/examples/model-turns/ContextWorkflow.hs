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

import Control.Lens (over, traversed, (^..))
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

-- Select only an admitted full tool-result body. Message text can also be
-- edited through `editableTexts`, but tool source/input and function arguments
-- have no editable selector.
trimToolResult :: ContextBlock -> ContextBlock
trimToolResult native@Native {contextNativeTexts = visibleTexts} =
  native {contextNativeTexts = map trimVisible visibleTexts}
  where
    trimVisible visibleText@C.ContextVisibleText
      { contextVisibleTextSelector = C.ToolResultText
      , contextVisibleTextEditable = True
      , contextVisibleTextText = original
      } = visibleText {contextVisibleTextText = trimBuildOutput original}
    trimVisible visibleText = visibleText
trimToolResult block = block

-- Use the traversal only when the intended edit spans every eligible body;
-- use the native selector above when the edit is limited to tool results.
trimEveryLongVisibleText :: Int -> Text -> Context -> Context
trimEveryLongVisibleText limit reason = over editableTexts trimLong
  where
    trimLong original
      | T.length original > limit =
          C.trimText reason (T.takeEnd limit original)
      | otherwise = original

-- Used as the body of a synchronous compiled tool or synchronous notebook
-- cell. Both the history edit and next-effort choice commit on whole-cell
-- success. Same-model continuation keeps opaque reasoning unchanged.
curateChild :: Member ContextReadWrite effects => Eff effects ()
curateChild = do
  modifyContext
    ( over contextBlocks
        (<> [Text Nothing User ("Child finding: " <> sharedFinding) []])
        . over contextBlocks (map trimToolResult)
    )
  C.setNextEffort C.High

-- Model and effort are staged by the same synchronous effect. Use a model
-- change only for a context/model combination the runtime has qualified;
-- incompatible opaque history must surface as an explicit refusal.
stageNextModelAndEffort :: Member ContextReadWrite effects => Text -> Eff effects ()
stageNextModelAndEffort alias = do
  setNextModel alias
  C.setNextEffort C.High

-- The caller supplies the actual installed tool definition. Each refusal keeps
-- its concrete spawned actor, when present, available for explicit recovery.
data ContextDelegation
  = ContextSpawnRefused SpawnError
  | ContextRequestRefused AgentRef RequestError
  | ContextRequested AgentRef (Request Text)
  deriving (Show)

curateAndDelegate
  :: ( Member ContextReadWrite effects, Member AgentLaunch effects, Member Replies effects
     , KnownEffects childEffects, KnownToolEffects childEffects, AsyncEffects childEffects
     , HasInstalledAgentApi tools childEffects
     )
  => AgentSpec tools childEffects
  -> Eff effects (Either CheckpointRefusal [ContextDelegation])
curateAndDelegate actualSpec = do
  curateChild
  captured <- checkpoint "curated context"
  case captured of
    Left issue -> pure (Left issue)
    Right context -> do
      idle <- mapM (\name -> spawnSubagent (ForkCtx context) SameDir
        ((defaultSpawnOptions actualSpec) { spawnLabel = Just name }))
        ["review-api", "review-tests"]
      admitted <- mapM activate (zip idle
        ["Review the API against: " <> sharedFinding, "Review the tests against: " <> sharedFinding])
      _ <- releaseCheckpoint context
      pure (Right admitted)
  where
    activate (Left issue, _) = pure (ContextSpawnRefused issue)
    activate (Right actor, input) = do
      admitted <- request @Text actor input defaultRequestOptions
      pure (either (ContextRequestRefused actor) (ContextRequested actor) admitted)

-- Read the transcript as bounded structural data, trim eligible tool-result
-- bodies, and convert selected nonopaque completed exchanges to notes with
-- their provenance. The optic receives full eligible text, never a preview.
inspectAndCurate
  :: Member ContextReadWrite effects
  => Eff effects Context
inspectAndCurate = do
  before <- getContext
  let completed =
        [ reference
        | block <- before ^.. contextBlocks . traversed
        , NativeProvenance reference <- [blockProvenance block]
        , blockKind block == NativeEvidence CompletedExchange
        ]
  modifyContext (toNotes completed . over contextBlocks (map trimToolResult))

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
