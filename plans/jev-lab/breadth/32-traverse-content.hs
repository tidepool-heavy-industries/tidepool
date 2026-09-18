{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
-- Same traversal, one variable changed: every branch now carries real content
-- instead of only its name. A directory is described by the function headers
-- of the Rust files inside it; a file by its own first lines. This is the
-- anchor text a web-traversal loop gets for free from a link and that a
-- directory listing does not have.

-- Real text for one child, whether it is a directory or a file.
peek :: Member Commands effs => Text -> Text -> Eff effs Text
peek oid path = do
  r <- Cmd.quiet (Cmd.run (Cmd.withArguments [oid, path]
        [bash|
          if git ls-tree --name-only "$1" "$2/" | grep -q .; then
            echo "DIRECTORY $2, containing:"
            for f in $(git ls-tree --name-only "$1" "$2/"); do
              echo "  $f:"
              git show "$1:$f" 2>/dev/null | grep -E '^[[:space:]]*(pub )?fn |^[[:space:]]*impl |KeyCode::' | head -n 6 | sed 's/^/    /'
            done
          else
            echo "FILE $2, beginning:"
            git show "$1:$2" 2>/dev/null | head -n 40
          fi
        |]))
  pure (T.take 1500 (either (const "") id (Cmd.stdout r)))

childrenWithContent :: Member Commands effs => Text -> Text -> Eff effs [(Text, Text)]
childrenWithContent oid path = do
  inner <- if T.null path
    then sh ["git", "ls-tree", "--name-only", oid]
    else sh ["git", "ls-tree", "--name-only", oid, path <> "/"]
  let entries = [ l | l <- T.lines inner, not (T.null l) ]
  mapM (\e -> (,) e <$> peek oid e) (take 10 entries)

hopC :: (Member Jev effs, Member Commands effs) => Text -> Text -> Text -> Eff effs (Text, Value)
hopC goal oid here = do
  kids <- childrenWithContent oid here
  if null kids
    then pure (here, object ["at" .= here, "note" .= ("leaf" :: Text)])
    else do
      let pool = J.pool #kids [ (k, String v, k) | (k, v) <- kids ]
          packet =
            #kids := pool
              :& #each := J.eachIn pool (\ref ->
                   #toward := J.askAbout ref
                     "Does the text of this entry contain, or lead to, the code that satisfies `goal`, rather than a sibling entry?"
                     :& Nil)
              :& Nil
      answer <- J.ask (J.state (object ["goal" .= goal, "current_node" .= (if T.null here then "(repository root)" else here)])) packet
      case answer of
        Left e -> pure (here, object ["jev_error" .= T.pack (show e)])
        Right r -> do
          let a = J.answers r
              scored = L.sortBy (\x y -> compare (snd y) (snd x)) [ (k, p.toward.yes) | (k, p) <- a.each ]
          pure ( fst (head scored)
               , object [ "from" .= (if T.null here then "(root)" else here)
                        , "ranked" .= [ object ["entry" .= k, "score" .= s] | (k, s) <- take 5 scored ] ] )

walkC :: (Member Jev effs, Member Commands effs) => Text -> Text -> Int -> Text -> [Value] -> Eff effs [Value]
walkC _ _ 0 _ acc = pure (reverse acc)
walkC goal oid n here acc = do
  (next, v) <- hopC goal oid here
  if next == here then pure (reverse (v : acc)) else walkC goal oid (n - 1) next (v : acc)

do
  t0 <- sh ["date", "+%s%3N"]
  steps <- walkC "the code that runs when the user presses the f key to cycle the item filter" "53ad43c" (3 :: Int) "" []
  t1 <- sh ["date", "+%s%3N"]
  let ms x = T.foldl (\acc c -> acc * 10 + (fromEnum c - 48)) 0 (T.filter (\c -> c >= '0' && c <= '9') x)
  pure (object ["elapsed_ms" .= (ms t1 - ms t0), "hops" .= steps])
