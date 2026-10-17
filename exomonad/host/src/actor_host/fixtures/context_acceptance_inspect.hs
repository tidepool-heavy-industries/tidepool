import qualified Tidepool.Agent.Context as C
import qualified ContextWorkflow as Workflow
C.getContext
do
  Right (_, C.ContextStaged _) <- Workflow.inspectAndCurate
  C.setNextReasoning (C.OpenAIReasoning C.OpenAIHigh)
  C.getContext
