{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module Project.RecordingReloadProbe where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import qualified Examples.JevComposedWorkflow as Example
import qualified Jev.Operators as J
import qualified Jev.Recording as Recording
import Tidepool.Effects.Core (Jev, FsRead, FsWrite)

payloadOffset :: Int
payloadOffset = 7

query :: Text -> J.Query (Int -> Int)
query evidence = J.taken <$> J.question (J.state (#evidence J.:= evidence)) #route
  (J.choice "Which retained continuation fits?"
    (J.alt #add "Add the authored offset." (+ payloadOffset)
      J..| J.alt #subtract "Subtract the authored offset." (subtract payloadOffset)))

observeAndSave :: (Member Jev effects, Member FsWrite effects)
  => Text -> Eff effects (Either Text Int)
observeAndSave path = do
  evaluation <- J.runQuery (query "unchanged evidence")
  stored <- Example.saveEvaluation path evaluation
  pure $ do
    either (Left . T.pack . show) Right stored
    action <- either (Left . T.pack . show) Right (J.result evaluation)
    pure (action 5)

replaySaved :: Member FsRead effects => Text -> Eff effects (Either Text Int)
replaySaved path = do
  loaded <- Example.loadRecordings path
  pure $ do
    archive <- either (Left . T.pack . show) Right loaded
    prepared <- either (Left . T.pack . show) Right
      (J.prepareQuery J.jevLatest (query "unchanged evidence"))
    action <- either (Left . T.pack . show) Right
      (J.result (J.replayQuery prepared (Recording.archiveRecords archive)))
    pure (action 5)

refusesChangedEvidence :: Member FsRead effects
  => Text -> Eff effects (Either Text Bool)
refusesChangedEvidence path = do
  loaded <- Example.loadRecordings path
  pure $ do
    archive <- either (Left . T.pack . show) Right loaded
    prepared <- either (Left . T.pack . show) Right
      (J.prepareQuery J.jevLatest (query "different evidence"))
    pure $ case J.result (J.replayQuery prepared (Recording.archiveRecords archive)) of
      Left (J.ReplayFailed (J.ChangedRequest 0)) -> True
      _ -> False
