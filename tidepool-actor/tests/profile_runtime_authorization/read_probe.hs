do
  outcome <- readFile "profile-probe.txt"
  case outcome of
    Left _ -> pure False
    Right contents -> pure (contents == "written by actor")
