import qualified Examples.JevComposedWorkflow as Composed
let intent = "repair the E0061 call"
evaluation <- Composed.observeBatch intent Composed.sampleContinuations
Right () <- Composed.saveEvaluation ".exomonad/state/jev-example.json" evaluation
evaluation
