normal <- Cmd.run [bash|printf normal|]
silent <- Cmd.quiet (Cmd.run [bash|printf quiet|])
Cmd.run [bash|printf normal-again|]
Cmd.stdout silent
Cmd.await (Cmd.job normal)
Cmd.output (Cmd.job normal)
