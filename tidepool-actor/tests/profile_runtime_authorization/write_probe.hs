do
  outcome <- writeFile "profile-probe.txt" "written by actor"
  case outcome of
    Left _ -> pure False
    Right () -> pure True
