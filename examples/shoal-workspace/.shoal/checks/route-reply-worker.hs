let destination = sessionReply
let wave = "implementation" :: ForkGroupLabel
let label = "feature" :: BranchLabel
candidate <- unfold (subgroup wave) (child @Candidate (solTask label sessionInput))
forwarding <- route (awaitSettledFork candidate) (\settled -> case settled of { ReplyAvailable answer -> do { _ <- reply destination (responseValue answer); pure () }; ReplyUnavailable failure -> error (T.pack (show failure)) })
