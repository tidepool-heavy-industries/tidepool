import qualified Examples.JevComposedWorkflow as Composed
import qualified Data.Text as Text
import Project.EvidencePattern
let intent = "repair the E0061 call"
evaluation <- Composed.observeBatch intent Composed.sampleContinuations
display evaluation
-- Keys, raw masses and original payload metadata include zero-mass rows.
-- Inspecting this projection does not execute any continuation.
let choices = fmap (Composed.distributionMetadata . fst) (J.result evaluation)
display choices
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
-- Supply the exact selected module/source identity when publishing this evidence.
-- The observed result records actual execution, separately from the distribution.
let evidenceFor sourceIdentity = Composed.decisionEvidence sourceIdentity intent Composed.sampleContinuations [("selected_continuation", Text.pack (show result))] evaluation
evaluation
