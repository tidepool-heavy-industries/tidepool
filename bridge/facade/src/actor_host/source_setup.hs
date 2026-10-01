data ProgressNote = ProgressNote Int (Int -> Int)
worker <- startAgent (withAgentLifetime ActorOwned (readonlyAgent "source-worker"))
let sourceLabel = [label|source-request|]
(answer, updates) <- do { issued <- requestWithProgress @ProgressNote @Int worker (assignment sourceLabel (10 :: Int)); Right () <- detachRequest (fst issued); pure issued }
