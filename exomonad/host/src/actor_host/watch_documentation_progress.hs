findingsState <- pollWatch findingsReady
reportState <- pollWatch reportReady
display (case findingsState of { WatchReady (ProgressUpdate (ProgressCursor 1) findings) -> findings == (["finding"] :: [Text]); _ -> False })
display (case reportState of { WatchPending _ -> True; _ -> False })
