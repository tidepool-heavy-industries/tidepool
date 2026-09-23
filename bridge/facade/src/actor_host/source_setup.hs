data ProgressNote = ProgressNote Int (Int -> Int)
worker <- startAgent (readonlyAgent "source-worker")
let sourceLabel = [label|source-request|]
(answer, updates) <- requestWithProgress @ProgressNote @Int worker (assignment sourceLabel (10 :: Int))
