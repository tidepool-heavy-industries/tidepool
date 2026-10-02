{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
{-# OPTIONS_GHC -Wno-simplifiable-class-constraints #-}

-- | Seed for synchronous transcript curation followed by inherited-context
-- delegation. The runtime commits the synchronous invocation before deferred
-- children begin, so both children see the curated transcript and this
-- module's persistent bindings.
module ContextWorkflow where

import Control.Lens (over)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import qualified Jev.Operators as J
import Jev.Tidepool ()

import Tidepool.Actors.Exomonad
import Tidepool.Agent.Context
import Tidepool.Agent.Assignment (assignment)
import Tidepool.Agent.Reply (Response)
import Tidepool.Effects.Core (ContextReadWrite, Jev)

sharedFinding :: Text
sharedFinding = "The cache key must include the selected transcript prefix."

-- Used as the body of a synchronous compiled tool or synchronous notebook
-- cell. New notes are ordinary editable user text with no inherited evidence.
curateChild :: Eff (ContextReadWrite ': ActorEffects) ()
curateChild = do
  modifyContext (over contextBlocks
    (<> [Text Nothing User ("Child finding: " <> sharedFinding) []]))
  setNextModel "executor"

-- The caller can persist the returned responses or attach watches. The two
-- child applications are explicitly actor-owned and inherit the parent's
-- committed transcript only after this invocation has settled.
curateAndDelegate
  :: Eff (ContextReadWrite ': ActorEffects) (Response Text, Response Text)
curateAndDelegate = do
  modifyContext (over contextBlocks
    (<> [Text Nothing User ("Parent-curated assignment. " <> sharedFinding) []]))
  unfoldDeferred (batch "context-curation" "review") $
    (,) <$> child @Text @CodingEffects @Text
      (withLifetime ActorOwned
        (coding projectHead (assignment [label|review-api|]
          ("Review the API against: " <> sharedFinding))))
      <*> child @Text @CodingEffects @Text
      (withLifetime ActorOwned
        (coding projectHead (assignment [label|review-tests|]
          ("Review the tests against: " <> sharedFinding))))

-- Jev judges each packet containing the original slice once. The returned
-- values come from the original rows, never from model-written replacements.
selectOriginalSlices
  :: Member Jev effects
  => [(Int, Text)]
  -> Eff effects [Text]
selectOriginalSlices originalSlices = do
  let packet = #selected J.:= J.each (\(offset, _) -> T.pack (show offset))
        (\(offset, original) -> #keep J.:= J.noul
          ("Keep this exact original slice at offset " <> T.pack (show offset) <> "?\n" <> original))
        originalSlices
  result <- J.ask (J.state (#purpose J.:= ("Select relevant source slices." :: Text))) packet
  case result of
    Left _ -> pure (map snd originalSlices)
    Right answer ->
      pure
        [ original
        | ((_, original), decision) <- answer.selected
        , J.holds J.careful decision.keep
        ]
