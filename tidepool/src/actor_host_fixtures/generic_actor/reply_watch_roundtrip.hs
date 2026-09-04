data ReplyReport = ReplyReport Int deriving (Show, Eq)
-- TIDEPOOL-ITEM --
data EchoReport = EchoReport Text deriving (Show, Eq)
-- TIDEPOOL-ITEM --
sharedDelta <- pure (1 :: Int)
-- TIDEPOOL-ITEM --
workers <- unfold (batch (case campaignLabel "reply-watch" of { Right value -> value; Left _ -> error "fixture campaign" }) (case forkGroupLabel "roundtrip" of { Right value -> value; Left _ -> error "fixture group" })) ((,) <$> child (researching @ReplyReport (case branchLabel "worker" of { Right value -> value; Left _ -> error "fixture branch" }) projectHead (41 :: Int)) <*> child (researching @EchoReport (case branchLabel "witness" of { Right value -> value; Left _ -> error "fixture branch" }) projectHead ("cache" :: Text)))
-- TIDEPOOL-ITEM --
initially <- (,) <$> pollResponse (forkedResponse (fst workers)) <*> pollResponse (forkedResponse (snd workers))
-- TIDEPOOL-ITEM --
readiness <- watch (case watchLabel "both-ready" of { Right value -> value; Left _ -> error "fixture watch" }) ((,) <$> awaitResponse (forkedResponse (fst workers)) <*> awaitResponse (forkedResponse (snd workers)))
