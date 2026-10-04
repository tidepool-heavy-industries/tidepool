do
  actor <- Check.root
  helper <- Check.readFile actor "helper"
  if helper == "false"
    then Check.assertThat "discarded false" False
    else if helper == "await"
      then Check.awaitCell actor "host assertion" "pure True"
      else Check.assertCell actor "host assertion" "True"
  pure (42 :: Int)
