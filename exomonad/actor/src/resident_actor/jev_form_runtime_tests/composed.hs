import Project.EvidencePattern
import qualified Tidepool.Aeson.Value as Json
import qualified Data.Text as Text
_ <- do
  let require condition message = if condition then pure () else error message
      intent = "repair the E0061 call"
      original = Composed.sampleContinuations
      changed = [row { evidenceValue = F.note "replacement-original" >> pure "replacement-original" } | row <- original]
  evaluation <- Composed.observeBatch intent original
  require (length (J.observations evaluation) == 1) "batch lost its observation"
  (answer, sufficient) <- either (error . tshow) pure (J.result evaluation)
  let allRows = J.weighted (Composed.fullDistribution answer)
  require (length allRows == 4) "full distribution lost an original alternative"
  require (length [() | (_, Selected row) <- allRows, evidenceSource row == "fixture:src/main.rs:83"] == 1)
    "distribution lost original typed evidence rows"
  require (length [() | (mass, Selected row) <- allRows, mass == 0, evidenceSource row == "fixture:src/main.rs:136"] == 1)
    "distribution dropped a zero-mass original row"
  let metadata = Composed.distributionMetadata answer
      keys = [key | (key, _, _) <- metadata]
  require (keys == ["none", "insufficient", "call", "other"])
    "exhaustive handlers lost authored key order"
  require ([(key, mass) | (key, mass, _) <- metadata, mass == 0] == [("other", 0)])
    "metadata dropped a zero-mass nonwinning payload"
  require (length [() | (key, _, facts) <- metadata,
      key == "call", lookup "source" facts == Just "fixture:src/main.rs:83"] == 1)
    "metadata lost the original payload source"
  let recorded = Json.encodeValue (Composed.decisionEvidence "issued-source-original"
        intent original [("execution", "not attempted")] evaluation)
  require (all (`Text.isInfixOf` recorded)
      ["issued-source-original", "execution_outcomes", "not attempted", "observation_archive", "fixture:src/main.rs:136"])
    "serializable evidence lost input, source, distribution or explicit execution status"
  first <- either (error . tshow) pure (J.customPolicy 0.6 0.2 0.7)
  second <- either (error . tshow) pure (J.customPolicy 0.9 0.5 0.9)
  let (accepted, doubtful) = Composed.compareBatchPolicies first second answer
  require (case accepted of Right _ -> True; Left _ -> False) "first caller policy should settle"
  require (case doubtful of Left _ -> True; Right _ -> False) "second caller policy should doubt"
  require (sufficient.yes > 0.8) "independent sufficiency answer lost"
  archive <- either (error . tshow) pure (Recording.decodeArchive
    (Recording.encodeArchive (Recording.recordingArchive (J.recordings evaluation))))
  let replayed = Composed.replayBatch J.jevLatest intent changed archive
      refused result = case J.result result of Left (J.ReplayFailed (J.ChangedRequest _)) -> True; _ -> False
  require (refused (Composed.replayBatch "changed-model" intent changed archive)) "changed model replay accepted"
  require (refused (Composed.replayBatch J.jevLatest "different intent" changed archive)) "changed evidence replay accepted"
  let differentQuestion = J.liftQuery (Composed.batchQuestionsWith "A changed question?" intent changed)
  require (refused (J.replayProgram J.BatchKnown J.jevLatest differentQuestion (Recording.archiveRecords archive))) "changed question replay accepted"
  (newAnswer, _) <- either (error . tshow) pure (J.result replayed)
  selection <- either (error . tshow) (pure . J.settledValue) (settleEvidence first newAnswer)
  result <- Composed.runSelection selection
  require (result == "replacement-original") "replay did not retain replacement payload"
  say "True"
