launchFresh :: Member Cmd.Commands effects => () -> Eff effects Cmd.Job
launchFresh () = Cmd.start (Cmd.argv ["pwd"])
launchRelative :: Member Cmd.Commands effects => () -> Eff effects Cmd.Job
launchRelative () = Cmd.start (Cmd.inDirectory "subdir" (Cmd.argv ["pwd"]))
launchFixed :: Member Cmd.Commands effects => () -> Eff effects Cmd.Job
launchFixed () = Cmd.start (Cmd.inDirectory fixedPath (Cmd.argv ["pwd"]))
