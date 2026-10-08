result <- Cmd.run (Cmd.bashCommand "git status --short")
display (Cmd.stdout result)
