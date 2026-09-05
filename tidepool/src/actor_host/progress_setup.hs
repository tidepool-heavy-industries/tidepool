data ProgressNote = ProgressNote Int (Int -> Int)
worker <- startAgent (readonlyAgent "progress-worker")
let Right progressRequestLabel = requestLabel "progress-request"
(answer, updates) <- requestWithProgress @ProgressNote @Int worker (requestOptions progressRequestLabel (10 :: Int))
let Right progressWatchLabel = watchLabel "progress-update"
observedUpdate <- watch progressWatchLabel (awaitProgressAfter updates (ProgressCursor 0))
let Right combinedLabel = watchLabel "progress-and-answer"
combined <- watch combinedLabel ((,) <$> awaitProgressAfter updates (ProgressCursor 0) <*> awaitValue answer)
let Right secondLabel = watchLabel "second-cursor"
secondCursor <- watch secondLabel (awaitProgressAfter updates (ProgressCursor 1))
