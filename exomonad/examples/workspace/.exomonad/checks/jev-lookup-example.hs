import qualified Examples.JevComposedWorkflow as Composed
import qualified Tidepool.Lookup.Tools as LookupTools
(captured, reassessment) <- Composed.captureThenAssess "Which API reads a file and detects stale writes?" (LookupTools.LookupArguments ["Tidepool.File.readSnapshot"])
display (Composed.captureSummary captured)
display (LookupTools.renderCapture captured)
display reassessment
-- Right False is insufficient; Left Doubt and a failed Evaluation differ.
display (fmap (fmap J.settledValue . J.judge J.careful) (J.result reassessment))
(captured, reassessment)
