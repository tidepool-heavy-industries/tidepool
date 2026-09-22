finalState <- pollWatch reportReady
retainedFindings <- pollWatch findingsReady
case finalState of { WatchReady (ReplyAvailable result) -> responseValue result == ("final report" :: Text); _ -> False }
case retainedFindings of { WatchReady (ProgressUpdate (ProgressCursor 1) findings) -> findings == (["finding"] :: [Text]); _ -> False }
