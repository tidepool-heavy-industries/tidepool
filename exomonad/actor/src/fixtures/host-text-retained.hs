textProof <-
  if tool_one == "output one" && tool_two == "output two" && textFirstProof == 41
    then pure (42 :: Int)
    else error "wrong retained Text payload"
