attachmentReply <- await (result attachmentJob)
display (case attachmentReply of { Right value -> value == (111 :: Int); _ -> False })
