import qualified Tidepool.Command as Cmd
do { job <- Cmd.background [bash|printf x >> restart-effect-executions|]; report <- waitFor (Cmd.awaitFinished job); display (show report) }
