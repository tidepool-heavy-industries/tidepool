{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
-- Continuous traversal: the repository-navigation analog of the published
-- web-traversal loop. Code lists the children of the current node; Jev picks
-- which one to descend into; code fetches that node's children; repeat.
-- Nothing generates a path, so no step can invent a file that does not exist.

-- Children of a node. A directory yields its entries; a Rust file yields its
-- function and impl headers with line numbers; anything else is a leaf.
childrenOf :: Member Commands effs => Text -> Text -> Eff effs [(Text, Text)]
childrenOf oid path = do
  inner <- if T.null path
    then sh ["git", "ls-tree", "--name-only", oid]
    else sh ["git", "ls-tree", "--name-only", oid, path <> "/"]
  let entries = [ l | l <- T.lines inner, not (T.null l) ]
  if not (null entries)
    then pure [ (e, "an entry in the directory " <> (if T.null path then "(repository root)" else path)) | e <- entries ]
    else do
      hdrs <- sh ["git", "grep", "-n", "-E", "^[[:space:]]*(pub )?fn |^[[:space:]]*impl ", oid, "--", path]
      pure [ (T.strip h, "a definition inside the file " <> path)
           | h <- take 24 (T.lines hdrs), not (T.null h) ]

-- One hop. Returns the chosen child's path, its mass and confidence, and
-- whether the model said the current node is already the answer.
hop :: (Member Jev effs, Member Commands effs) => Text -> Text -> Text -> Eff effs (Text, Value)
hop goal oid here = do
  kids <- childrenOf oid here
  if null kids
    then pure (here, object ["at" .= here, "note" .= ("no children; this is a leaf" :: Text)])
    else do
      -- `git ls-tree` already yields paths from the repository root, so the
      -- chosen key is the next node as it stands.
      let nameOf k = k
          pool = J.pool #kids [ (k, String (k <> " -- " <> d), k) | (k, d) <- kids ]
          packet =
            #kids := pool
              :& #arrived := J.noul
                   "Does `current_node` name a single function or a single line of code, rather than a directory or a whole file?"
              :& #each := J.eachIn pool (\ref ->
                   #toward := J.askAbout ref
                     "Would the code that satisfies `goal` be found inside this entry, rather than in a sibling entry?"
                     :& Nil)
              :& Nil
      answer <- J.ask
        (J.state (object ["goal" .= goal, "current_node" .= (if T.null here then "(repository root)" else here)]))
        packet
      case answer of
        Left e -> pure (here, object ["jev_error" .= T.pack (show e)])
        Right r -> do
          let a = J.answers r
              scored = [ (k, p.toward.yes) | (k, p) <- a.each ]
              best = L.maximumBy (\x y -> compare (snd x) (snd y)) scored
              runner = case L.sortBy (\x y -> compare (snd y) (snd x)) scored of
                (_ : s : _) -> snd s
                _ -> 0
          pure ( nameOf (fst best)
               , object [ "from" .= (if T.null here then "(root)" else here)
                        , "chose" .= fst best, "its_score" .= snd best
                        , "runner_up" .= runner, "arrived" .= a.arrived.yes
                        , "considered" .= length scored ] )

walk :: (Member Jev effs, Member Commands effs) => Text -> Text -> Int -> Text -> [Value] -> Eff effs [Value]
walk _ _ 0 _ acc = pure (reverse acc)
walk goal oid n here acc = do
  (next, v) <- hop goal oid here
  if next == here then pure (reverse (v : acc)) else walk goal oid (n - 1) next (v : acc)

do
  t0 <- sh ["date", "+%s%3N"]
  steps <- walk "the code that runs when the user presses the f key to cycle the item filter" "53ad43c" (4 :: Int) "" []
  t1 <- sh ["date", "+%s%3N"]
  let ms x = T.foldl (\acc c -> acc * 10 + (fromEnum c - 48)) 0 (T.filter (\c -> c >= '0' && c <= '9') x)
  pure (object ["elapsed_ms" .= (ms t1 - ms t0), "hops" .= steps])
