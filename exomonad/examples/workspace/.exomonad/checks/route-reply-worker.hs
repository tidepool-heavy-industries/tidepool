{-# LANGUAGE QuasiQuotes #-}
let destination = respond
let wave = "implementation" :: ForkGroupLabel
let label = [label|feature|]
candidate <- unfold (subgroup wave) (child @Candidate (solTask label Medium sessionInput))
forwarding <- route (awaitSettled candidate) (\settled -> case settled of { ReplyAvailable answer -> void (destination (responseValue answer)); ReplyUnavailable failure -> error (T.pack (show failure)) })
