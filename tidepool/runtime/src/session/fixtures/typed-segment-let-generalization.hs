let segmentIdentity value = value
answer <- pure (let unused = error "unused inner bottom" :: Int in 7 :: Int)
record answer
record (segmentIdentity (11 :: Int))
record (if segmentIdentity True then 13 else 0)
