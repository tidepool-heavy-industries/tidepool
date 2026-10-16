import qualified Tidepool.Agent.Context as C
import qualified ContextWorkflow as Workflow
C.getContext
do
  Right (_, C.ContextStaged _) <- Workflow.inspectAndCurate
  C.setNextEffort C.High
  C.getContext
