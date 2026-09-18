wsRoot = "/home/inanna/dev/shoal-evals/tui-test-app"
oidA = "f726882"

sh args = Cmd.quiet (Cmd.run (Cmd.inDirectory wsRoot (Cmd.argv args))) <&> (either (const "") id . Cmd.stdout)

backticked h = case T.splitOn "`" h of { (_ : x : _) -> x ; _ -> "" }
fileOf loc = T.takeWhile (/= ':') loc
lineOf loc = case T.splitOn ":" loc of { (_ : l : _) -> l ; _ -> "0" }

symbolA = backticked (fst (fst (head (mechanicalGroups rawA))))
leafSymA = last (T.splitOn "::" symbolA)
reportedA = concatMap primaries (snd (head (mechanicalGroups rawA)))
touchedA = L.nub (map fileOf reportedA)

sweepA <- sh ["git", "grep", "-n", leafSymA, oidA, "--", "src"]
recentA <- traverse (\f -> (,) f <$> sh ["git", "log", "-1", "--format=%h %s", oidA, "--", f]) touchedA
object [ "symbol" .= symbolA, "leaf" .= leafSymA, "reported_sites" .= reportedA, "files" .= touchedA
       , "sweep_hits" .= length (T.lines sweepA), "sweep" .= take 30 (T.lines sweepA)
       , "recent" .= [ object ["file" .= f, "last" .= T.strip b] | (f, b) <- recentA ] ]
