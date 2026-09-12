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
workers <- unfold (batch "reply-watch" "roundtrip") ((,,) <$> child (withBranchDeadline (after (minutes 5)) (researching @ReplyReport "worker" projectHead (41 :: Int))) <*> child (researching @EchoReport "witness" projectHead ("cache" :: Text)) <*> child (coding @ScaffoldReport "scaffold" projectHead ("recursive" :: Text)))
-- TIDEPOOL-ITEM --
initially <- (,) <$> pollResponse (forkedResponse (first3 workers)) <*> pollResponse (forkedResponse (second3 workers))
-- TIDEPOOL-ITEM --
readiness <- watch "both-ready" ((,) <$> awaitResponse (forkedResponse (first3 workers)) <*> awaitResponse (forkedResponse (second3 workers)))
