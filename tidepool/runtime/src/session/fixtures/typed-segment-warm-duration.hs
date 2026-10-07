warmDurationResult <- pure (if Duration.seconds (fromIntegral baselinePlannedValue) == Duration.minutes 1 then (0 :: Int) else 7)
segmentRecord warmDurationResult
