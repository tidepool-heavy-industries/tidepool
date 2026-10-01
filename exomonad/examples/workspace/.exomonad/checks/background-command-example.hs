-- Stage: imports
import qualified Tidepool.Command as Cmd
import Project.BackgroundCommandExample
-- Stage: await
job <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv ["sh", "-c", "printf passed"]))
commandEvidence <- awaitCommandEvidence job
-- Stage: compact-query
completionProjection job commandEvidence
-- Stage: full-evidence
commandEvidence
