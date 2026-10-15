import qualified Examples.JevComposedWorkflow as Composed
import qualified Jev.Recording as Recording
import Project.EvidencePattern
Right archive <- Composed.loadRecordings ".exomonad/state/jev-example.json"
-- Reconstruct from the currently selected source, including its current payloads.
let intent = "repair the E0061 call"
let replayed = Composed.replayBatch J.jevLatest intent Composed.sampleContinuations archive
display replayed
-- These are pure controls and make zero provider calls.
let changedPayloads = [row { evidenceValue = pure "edited continuation" } | row <- Composed.sampleContinuations]
let payloadOnly = Composed.replayBatch J.jevLatest intent changedPayloads archive
let changedEvidence = Composed.replayBatch J.jevLatest "different task evidence" changedPayloads archive
let changedModel = Composed.replayBatch "different-model" intent changedPayloads archive
let changedQuestion = J.replayProgram J.BatchKnown J.jevLatest (J.liftQuery (Composed.batchQuestionsWith "A different question?" intent changedPayloads)) (Recording.archiveRecords archive)
display (payloadOnly, changedEvidence, changedModel, changedQuestion)
replayed
