data ReplyReport = ReplyReport Int deriving (Show, Eq)
-- TIDEPOOL-ITEM --
worker <- startAgent (readonlyAgent "reply-watch-worker")
-- TIDEPOOL-ITEM --
response <- request @ReplyReport worker "Increment the typed input." (41 :: Int)
-- TIDEPOOL-ITEM --
initially <- pollResponse response
-- TIDEPOOL-ITEM --
readiness <- watch (awaitResponse response)
