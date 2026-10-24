sameStageExcluded <- pure (7 :: Int)
sameStageExcludedToo <- pure (13 :: Int)
data SameStageTag = SameStageTag
sameStageUnused <- pure (17 :: Int)
sameStageSeed <- pure (23 :: Int)
sameStageRead <- pure (sameStageSeed + 1)
sameStageSeed <- pure (91 :: Int)
sameStageResult <- pure (sameStageRead + sameStageSeed)
sameStageFuture <- pure (11 :: Int)
