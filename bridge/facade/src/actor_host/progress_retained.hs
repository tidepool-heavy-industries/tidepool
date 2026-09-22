closed <- pollProgress updates
case closed of { ProgressClosed -> True; _ -> False }
case captured of { WatchReady (ProgressUpdate _ (ProgressNote _ f)) -> f 5; _ -> error "lost snapshot" }
lateSnapshot <- pollWatch lateWatch
case lateSnapshot of { WatchReady (ProgressUpdate _ (ProgressNote _ f)) -> f 5; _ -> error "lost unobserved snapshot" }
combinedResult <- pollWatch combined
case combinedResult of { WatchReady (ProgressUpdate (ProgressCursor 1) (ProgressNote 1 f), 42) -> f 6; _ -> error "combined watch replaced its captured value" }
