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
workers <- unfold (batch (case campaignLabel "reply-watch" of { Right value -> value; Left _ -> error "fixture campaign" }) (case forkGroupLabel "roundtrip" of { Right value -> value; Left _ -> error "fixture group" })) ((,,) <$> child (researching @ReplyReport (case branchLabel "worker" of { Right value -> value; Left _ -> error "fixture branch" }) projectHead (41 :: Int)) <*> child (researching @EchoReport (case branchLabel "witness" of { Right value -> value; Left _ -> error "fixture branch" }) projectHead ("cache" :: Text)) <*> child (scaffolding @ScaffoldReport (case branchLabel "scaffold" of { Right value -> value; Left _ -> error "fixture branch" }) projectHead ("recursive" :: Text)))
-- TIDEPOOL-ITEM --
initially <- (,) <$> pollResponse (forkedResponse (first3 workers)) <*> pollResponse (forkedResponse (second3 workers))
-- TIDEPOOL-ITEM --
readiness <- watch (case watchLabel "both-ready" of { Right value -> value; Left _ -> error "fixture watch" }) ((,) <$> awaitResponse (forkedResponse (first3 workers)) <*> awaitResponse (forkedResponse (second3 workers)))
