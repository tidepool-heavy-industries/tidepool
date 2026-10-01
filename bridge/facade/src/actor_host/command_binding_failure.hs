observed <- Cmd.await retainedBeforeFailure
(Cmd.commandOutcome (Cmd.commandResult observed), Cmd.capturedOutput observed, Cmd.stdout observed, Cmd.stderr observed)
