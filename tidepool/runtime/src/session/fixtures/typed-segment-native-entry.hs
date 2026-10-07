firstPlannedValue <- pure (baselinePlannedValue + 1)
sameSegmentFuture <- pure (firstPlannedValue + 9)
boundaryCapture :: Int
boundaryCapture = firstPlannedValue
secondPlannedValue <- pure (boundaryCapture + 1)
segmentRecord secondPlannedValue
