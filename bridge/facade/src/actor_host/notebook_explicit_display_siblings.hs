data DisplayPair = DisplayPair { left :: Text, right :: Text }
pair <- do
  job <- Cmd.quiet (Cmd.run (Cmd.argv ["cat", "display.txt"]))
  let value = either (const "") id (Cmd.stdout job)
  pure (DisplayPair value value)
shown <- display pair
expanded <- expand shown (fst (head (expansions shown)))
