42 :: Int
quoted <- pure (Q.describe [Q.bash|printf transaction-local-quoter|])
42 :: Int
