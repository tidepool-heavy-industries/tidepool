data ProgressNote = ProgressNote Int (Int -> Int)
worker <- startAgent (readonlyAgent "source-worker")
let Right sourceRequestLabel = requestLabel "source-request"
(answer, updates) <- requestWithProgress @ProgressNote @Int worker (requestOptions sourceRequestLabel (10 :: Int))
