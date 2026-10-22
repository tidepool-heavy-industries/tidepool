textFirstProof <-
  if tool_one == "output one"
    then pure (41 :: Int)
    else error "wrong first Text payload"
data UnrelatedTextPublication = UnrelatedTextPublication
displayFirstProof <-
  case PriorDisplay.displayWith 256 UnrelatedTextPublication of
    (rendered, False) | rendered == "UnrelatedTextPublication" -> pure (43 :: Int)
    _ -> error "generated Display did not retain its prior Source graph"
