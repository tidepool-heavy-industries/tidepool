data ProgressNote = ProgressNote Int (Int -> Int)
worker <- startAgent (withAgentLifetime ActorOwned (readonlyAgent "progress-worker"))
let progressLabel = [label|progress-request|]
(answer, updates) <- do { issued <- requestWithProgress @ProgressNote @Int worker (assignment progressLabel (10 :: Int)); Right () <- detachRequest (fst issued); pure issued }
let progressWatchLabel = "progress-update" :: WatchLabel
observedUpdate <- watch progressWatchLabel (awaitProgressAfter updates (ProgressCursor 0))
let combinedLabel = "progress-and-answer" :: WatchLabel
combined <- watch combinedLabel ((,) <$> awaitProgressAfter updates (ProgressCursor 0) <*> awaitValue answer)
let secondLabel = "second-cursor" :: WatchLabel
secondCursor <- watch secondLabel (awaitProgressAfter updates (ProgressCursor 1))
