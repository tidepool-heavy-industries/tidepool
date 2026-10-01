normal <- Cmd.run [bash|printf normal|]
silent <- Cmd.quiet (Cmd.run [bash|printf quiet|])
Cmd.run [bash|printf normal-again|]
Cmd.stdout silent
again <- Cmd.await (Cmd.job normal)
Cmd.stdout again
Cmd.observe (Cmd.Observation 0 1024) (Cmd.job normal)
Cmd.output (Cmd.job normal)
