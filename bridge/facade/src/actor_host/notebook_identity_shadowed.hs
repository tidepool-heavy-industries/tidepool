data Version = NewVersion Bool deriving Show
retained <- pure old
fresh <- pure (NewVersion True)
mapping <- pure (Map.singleton (1 :: Int) retained)
(Map.lookup 1 mapping, fresh)
