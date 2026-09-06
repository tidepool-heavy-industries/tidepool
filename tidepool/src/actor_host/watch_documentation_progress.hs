findingsState <- pollWatch findingsReady
reportState <- pollWatch reportReady
case findingsState of { WatchReady (ProgressUpdate (ProgressCursor 1) findings) -> findings == (["finding"] :: [Text]); _ -> False }
case reportState of { WatchPending -> True; _ -> False }
