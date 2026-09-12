data ProgressNote = ProgressNote Int (Int -> Int)
worker <- startAgent (readonlyAgent "progress-worker")
let progressRequestLabel = "progress-request" :: RequestLabel
(answer, updates) <- requestWithProgress @ProgressNote @Int worker (requestOptions progressRequestLabel (10 :: Int))
let progressWatchLabel = "progress-update" :: WatchLabel
observedUpdate <- watch progressWatchLabel (awaitProgressAfter updates (ProgressCursor 0))
let combinedLabel = "progress-and-answer" :: WatchLabel
combined <- watch combinedLabel ((,) <$> awaitProgressAfter updates (ProgressCursor 0) <*> awaitValue answer)
let secondLabel = "second-cursor" :: WatchLabel
secondCursor <- watch secondLabel (awaitProgressAfter updates (ProgressCursor 1))
