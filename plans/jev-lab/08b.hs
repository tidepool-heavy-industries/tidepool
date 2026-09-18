sweepA <- sh "/home/inanna/dev/shoal-evals/tui-test-app" ["git", "grep", "-n", leafA, "f726882", "--", "src"]
recentA <- traverse (\f -> (,) f <$> sh "/home/inanna/dev/shoal-evals/tui-test-app" ["git", "log", "-1", "--format=%h %s", "f726882", "--", f]) filesA
object [ "symbol" .= symA, "leaf" .= leafA, "files" .= filesA
       , "sweep_hits" .= length (T.lines sweepA), "sweep" .= take 30 (T.lines sweepA)
       , "recent" .= [ object ["file" .= f, "last" .= T.strip b] | (f, b) <- recentA ] ]
