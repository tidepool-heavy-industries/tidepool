do
  answer <- deliberate "Check whether the supplied integer is forty-one." (Node 41 [])
  pure (if answer then 84 else 0)
