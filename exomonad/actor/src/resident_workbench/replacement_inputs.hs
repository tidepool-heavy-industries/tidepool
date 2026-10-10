let replacementEntry :: Int -> Eff '[] Int
    replacementEntry state = pure (if state == 41 then 42 else error "replacement lost its predecessor checkpoint")
    replacementCheckpoint :: Int
    replacementCheckpoint = 41
