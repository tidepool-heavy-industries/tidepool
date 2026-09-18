ws = "/home/inanna/dev/shoal-evals/tui-test-app"

blobAt oid path = sh ws ["git", "show", oid <> ":" <> path]

excerptFrom body n radius =
  let ls = T.lines body
      lo = max 0 (n - radius - 1)
  in T.unlines [ T.pack (show (i + lo + 1)) <> ": " <> l | (i, l) <- zip [0 ..] (take (2 * radius + 1) (drop lo ls)) ]

srcFiles = L.nub (map fileOf ("src/app.rs:61:10" : (concatMap primaries (snd (head (mechanicalGroups rawA))))) ++ map (\h -> fileOf (T.drop 1 (T.dropWhile (/= ':') h))) (T.lines sweepA))
srcFiles
