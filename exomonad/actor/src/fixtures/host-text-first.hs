textFirstProof <-
  if tool_one == "output one"
    then pure (41 :: Int)
    else error "wrong first Text payload"
data UnrelatedTextPublication = UnrelatedTextPublication
