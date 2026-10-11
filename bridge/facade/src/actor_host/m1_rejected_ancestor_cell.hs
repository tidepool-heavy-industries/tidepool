_ <- do
  result <- Cmd.run [bash|IFS= read -r release < ancestor-output-release && test "$release" = released|]
  Nothing <- pure (Cmd.failure result)
  display (40 + 2 :: Int)
