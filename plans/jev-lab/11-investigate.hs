lineNo loc = case T.splitOn ":" loc of { (_ : l : _) -> T.foldl (\a c -> a * 10 + (fromEnum c - 48)) (0 :: Int) (T.filter (\c -> c >= '0' && c <= '9') l) ; _ -> 0 }

investigate wsDir oid owned cmd code raw = do
  let gs = mechanicalGroups raw
      sym = backticked (fst (fst (head gs)))
      leaf = last (T.splitOn "::" sym)
      reported = concatMap primaries (snd (head gs))
      shared = [ T.strip (snd (T.breakOn " @ " s)) | s <- snd (fst (head gs)) ]
      sharedLocs = map (T.drop 3) shared
  verdict <- askNarrow owned cmd code gs
  sweep <- sh wsDir ["git", "grep", "-n", leaf, oid, "--", "src"]
  let hits = [ T.drop 1 (T.dropWhile (/= ':') h) | h <- T.lines sweep ]
      locs = [ ("the definition the compiler pointed at", l) | l <- sharedLocs ]
          ++ [ ("reported by the compiler", l) | l <- reported ]
          ++ [ ("found by searching the tree for " <> leaf, l) | l <- hits, l `notElem` reported ]
      files = L.nub (map (T.takeWhile (/= ':')) (map snd locs))
  bodies <- traverse (\f -> (,) f <$> blobAt oid f) files
  let excerpt l = maybe "<unavailable>" (\b -> excerptFrom b (lineNo l) 4) (lookup (T.takeWhile (/= ':') l) bodies)
      keyed = [ ("L" <> T.pack (show n), w, l) | (n, (w, l)) <- zip [1 :: Int ..] locs ]
      render (k, w, l) = k <> "| " <> l <> "  (" <> w <> ")\n" <> excerpt l
      lpool = J.pool #locations [ (k, String (render kwl), l) | kwl@(k, w, l) <- keyed ]
      packet = #locations J.:= lpool
        J.:& #grounded J.:= J.noul ("Does `locations` contain Rust source excerpts that mention " <> leaf <> "?")
        J.:& #each J.:= J.eachIn lpool (\ref ->
             #must_change J.:= J.askAbout ref ("To make `command` succeed, must the code shown at this location be edited?")
               J.:& #already_handles J.:= J.askAbout ref ("Does the code shown at this location already handle " <> sym <> "?")
               J.:& #is_test J.:= J.askAbout ref "Is the code shown at this location inside a test?"
               J.:& #declares J.:= J.askAbout ref ("Does this location declare " <> sym <> " rather than consume it?")
               J.:& J.Nil)
        J.:& J.Nil
  found <- J.ask (J.state (object
      [ "command" .= cmd, "exit_status" .= code, "owned_paths" .= owned, "symbol" .= sym
      , "locations" .= T.intercalate "\n" [ render kwl | kwl <- keyed ] ])) packet
  pure (sym, leaf, keyed, verdict, found)
