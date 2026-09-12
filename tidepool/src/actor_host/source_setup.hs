data ProgressNote = ProgressNote Int (Int -> Int)
worker <- startAgent (readonlyAgent "source-worker")
let sourceRequestLabel = "source-request" :: RequestLabel
(answer, updates) <- requestWithProgress @ProgressNote @Int worker (requestOptions sourceRequestLabel (10 :: Int))
