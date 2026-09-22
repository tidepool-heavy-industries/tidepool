data ReplyReport = ReplyReport Int deriving (Show, Eq)
-- TIDEPOOL-ITEM --
data EchoReport = EchoReport Text deriving (Show, Eq)
-- TIDEPOOL-ITEM --
data ScaffoldReport = ScaffoldReport Text deriving (Show, Eq)
-- TIDEPOOL-ITEM --
first3 (value, _, _) = value
second3 (_, value, _) = value
third3 (_, _, value) = value
-- TIDEPOOL-ITEM --
sharedDelta <- pure (1 :: Int)
-- TIDEPOOL-ITEM --
workers <- unfold (batch "reply-watch" "roundtrip") ((,,) <$> child (researching @ReplyReport projectHead ((assignment "worker" (41 :: Int)) { deadline = Just (minutes 5) })) <*> child (researching @EchoReport projectHead (assignment "witness" ("cache" :: Text))) <*> child (coding @ScaffoldReport projectHead (assignment "scaffold" ("recursive" :: Text))))
-- TIDEPOOL-ITEM --
initially <- (,) <$> pollResponse (first3 workers) <*> pollResponse (second3 workers)
-- TIDEPOOL-ITEM --
readiness <- watch "both-ready" ((,) <$> awaitResponse (first3 workers) <*> awaitResponse (second3 workers))
