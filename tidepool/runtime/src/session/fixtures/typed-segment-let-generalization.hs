let segmentIdentity value = value
answer <- pure (let unused = error "unused inner bottom" :: Int in 7 :: Int)
segmentRecord answer
segmentRecord (segmentIdentity (11 :: Int))
segmentRecord (if segmentIdentity True then 13 else 0)
