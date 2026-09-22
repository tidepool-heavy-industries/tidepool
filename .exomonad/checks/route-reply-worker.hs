let destination = sessionReply
let Right wave = forkGroupLabel "implementation"
let Right label = branchLabel "feature"
candidate <- unfold (subgroup wave) (child @Candidate (solTask label sessionInput))
forwarding <- route (awaitSettledFork candidate) (\settled -> case settled of { ReplyAvailable answer -> do { _ <- reply destination (responseValue answer); pure () }; ReplyUnavailable failure -> error (T.pack (show failure)) })
