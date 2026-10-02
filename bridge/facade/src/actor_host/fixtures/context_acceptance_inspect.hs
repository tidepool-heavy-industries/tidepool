import qualified Tidepool.Agent.Context as C
import qualified ContextWorkflow as Workflow
C.getContext
do
  _ <- Workflow.inspectAndCurate
  Workflow.curateChild
  C.getContext
