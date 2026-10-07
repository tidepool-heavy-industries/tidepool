do
  refused <- Cmd.run (Cmd.argv ["true"])
  let result = Cmd.commandResult refused
  case (Cmd.commandOutcome result, Cmd.commandCleanup result) of
    (Cmd.CommandFailed _, Cmd.CommandClean) -> pure ()
    _ -> error "HOSTED_COMMAND_RESOURCE_REFUSAL_REQUIRED"
