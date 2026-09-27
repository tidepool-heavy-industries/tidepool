-- Stage: imports
import qualified Tidepool.Command as Cmd
import Project.BackgroundCommandExample
-- Stage: start
job <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv ["sh", "-c", "printf passed"]))
-- Stage: watch
watcher <- startCommandWatcher job
-- Stage: compact-query
readCommandProjection watcher
-- Stage: full-evidence
readCommandEvidence watcher
-- Stage: cleanup
finishCommandWatcher watcher
