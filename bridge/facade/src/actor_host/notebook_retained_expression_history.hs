41 :: Int
pure (42 :: Int)
do
  job <- Cmd.quiet (Cmd.run (Cmd.argv ["cat", "history.txt"]))
  pure (either (const "") id (Cmd.stdout job))
