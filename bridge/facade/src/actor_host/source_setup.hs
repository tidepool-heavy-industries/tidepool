data ProgressNote = ProgressNote Int (Int -> Int)
worker <- startAgent (readonlyAgent "source-worker")
let sourceLabel = "source-request" :: Label
(answer, updates) <- requestWithProgress @ProgressNote @Int worker (assignment sourceLabel (10 :: Int))
