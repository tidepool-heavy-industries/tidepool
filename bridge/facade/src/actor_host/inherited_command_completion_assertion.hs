completed <- Cmd.await job
if Cmd.commandOutcome (Cmd.commandResult completed) == Cmd.CommandExited 0
  then pure ()
  else error "inherited command completion outcome mismatch"
