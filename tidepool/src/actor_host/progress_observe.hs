captured <- pollWatch observedUpdate
latest <- pollProgress updates
let result = case (captured, latest) of { (WatchReady (ProgressUpdate _ (ProgressNote 1 f)), ProgressUpdate _ (ProgressNote 2 g)) -> (f 3, g 3); _ -> error "wrong progress snapshots" }
result
let lateLabel = "late-progress" :: WatchLabel
lateWatch <- watch lateLabel (awaitProgressAfter updates (ProgressCursor 1))
secondObserved <- pollWatch secondCursor
case secondObserved of { WatchReady (ProgressUpdate (ProgressCursor 2) (ProgressNote 2 f)) -> f 4; _ -> error "independent cursor lost update" }
