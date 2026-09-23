b <- lookupRaw (LookupRequest ["Cmd.quiet", "J.settle"] False Nothing 3 [])
map (\r -> r) (lookupResults b)
