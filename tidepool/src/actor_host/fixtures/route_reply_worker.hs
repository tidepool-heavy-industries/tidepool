let destination = sessionReply
let Right campaign = campaignLabel "route-reply-child"
let Right wave = forkGroupLabel "implementation"
let Right label = branchLabel "feature"
candidate <- implement (batch campaign wave) label boundHead sessionInput
forwarding <- route (awaitSettledFork candidate) (\settled -> case settled of { ReplyAvailable answer -> do { _ <- reply destination (responseValue answer); pure () }; ReplyUnavailable failure -> error (T.pack (show failure)) })
