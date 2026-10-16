finalState <- pollWatch reportReady
retainedFindings <- pollWatch findingsReady
display (case finalState of { WatchReady (Right result) -> result == ("final report" :: Text); _ -> False })
display (case retainedFindings of { WatchReady (ProgressUpdate (ProgressCursor 1) findings) -> findings == (["finding"] :: [Text]); _ -> False })
