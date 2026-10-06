do
  let observed = (__EXPRESSION__ :: Int)
  if observed == __EXPECTED__
    then pure observed
    else error "native integer observation guard failed"
