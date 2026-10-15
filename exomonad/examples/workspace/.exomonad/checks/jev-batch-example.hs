import qualified Examples.JevComposedWorkflow as Composed
import Project.EvidencePattern
let intent = "repair the E0061 call"
evaluation <- Composed.observeBatch intent Composed.sampleContinuations
display evaluation
let choices = fmap (\(answer, _) -> J.weighted (Composed.fullDistribution answer)) (J.result evaluation)
display (fmap (map (\(mass, row) -> (mass, case row of Selected evidence -> evidenceKey evidence; NoUsefulEvidence -> "none"; InsufficientEvidence -> "insufficient"))) choices)
Right ordinary <- pure (J.customPolicy 0.6 0.2 0.7)
Right cautious <- pure (J.customPolicy 0.9 0.5 0.9)
let policies = fmap (\(answer, _) -> Composed.compareBatchPolicies ordinary cautious answer) (J.result evaluation)
display policies
-- Run this separately when ready to execute a selected original continuation.
result <- case J.result evaluation of
  Left failure -> pure (Left (Left failure))
  Right (answer, _) -> case settleEvidence ordinary answer of
    Left doubt -> pure (Left (Right doubt))
    Right selected -> Right <$> Composed.runSelection (J.settledValue selected)
display result
evaluation
