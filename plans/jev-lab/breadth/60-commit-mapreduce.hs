{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
import Tidepool.Effects.Core (Jev, Commands)

sh :: Member Commands effs => [Text] -> Eff effs Text
sh args = do
  r <- Cmd.run (Cmd.argv args)
  pure (either (const "") id (Cmd.stdout r))

anchorUserHash :: Text
anchorUserHash = "60e8e8050"

anchorUserSubject :: Text
anchorUserSubject = "fix(shoal): a refused command and a refused worktree say what was refused"

anchorInternalHash :: Text
anchorInternalHash = "25fe148d7"

anchorInternalSubject :: Text
anchorInternalSubject = "style(runtime): run cargo fmt on prepared_execution.rs"

parseLine :: Text -> Maybe (Text, Text)
parseLine l = case T.breakOn "|" l of
  (h, rest) | not (T.null rest) -> Just (h, T.drop 1 rest)
  _ -> Nothing

chunkOf :: Int -> [a] -> [[a]]
chunkOf _ [] = []
chunkOf n xs = let (a, b) = P.splitAt n xs in a : chunkOf n b

batchAsk :: Member Jev effs => Int -> [(Text, Text)] -> Eff effs (Either Text (Double, Double, Double, Double, [(Text, Double, Double)]))
batchAsk idx items = do
  let realItems = [ (h, s) | (h, s) <- items, h /= anchorUserHash, h /= anchorInternalHash ]
      poolItems = [(anchorUserHash, anchorUserSubject), (anchorInternalHash, anchorInternalSubject)] ++ realItems
      pool = J.pool #items [ (h, String s, h) | (h, s) <- poolItems ]
      packet =
        #items := pool
          :& #each := J.eachIn pool (\ref ->
               #user_visible := J.askAbout ref "Does this commit subject describe a change a user of the system would notice, rather than an internal refactor, test change or documentation edit?"
                 :& #names_component := J.askAbout ref "Does this commit subject name a specific component, file or subsystem?"
                 :& Nil)
          :& Nil
  answer <- J.ask (J.state (object ["task" .= ("classify real commit subjects for user visibility and component naming, batch " <> T.pack (show idx) :: Text)])) packet
  case answer of
    Left e -> pure (Left (T.pack (show e)))
    Right r -> do
      let a = J.answers r
          rows = [ (k, p.user_visible.yes, p.names_component.yes) | (k, p) <- a.each ]
          findProb h = case [ (uv, nc) | (k, uv, nc) <- rows, k == h ] of
            ((uv, nc) : _) -> (uv, nc)
            [] -> (-1, -1)
          (auUv, auNc) = findProb anchorUserHash
          (aiUv, aiNc) = findProb anchorInternalHash
      pure (Right (auUv, auNc, aiUv, aiNc, rows))

do
  txt <- sh ["cat", "/home/inanna/.claude/jobs/4940a626/tmp/lab8/commits-300.txt"]
  let ls = filter (not . T.null) (T.lines txt)
      items = [ (h, s) | l <- ls, Just (h, s) <- [parseLine l] ]
      subjectOf h = case [ s | (h', s) <- items, h' == h ] of
        (s : _) -> s
        [] -> ""
      batches = chunkOf 25 items
  results <- forM (P.zip [1 :: Int ..] batches) (\(i, b) -> batchAsk i b)
  let anchorTable =
        [ case res of
            Left e -> object ["batch" .= i, "error" .= e]
            Right (auUv, auNc, aiUv, aiNc, _) -> object
              [ "batch" .= i
              , "anchor_user_visible" .= auUv
              , "anchor_user_names_component" .= auNc
              , "anchor_internal_visible" .= aiUv
              , "anchor_internal_names_component" .= aiNc
              ]
        | (i, res) <- P.zip [1 :: Int ..] results
        ]
      allRows = P.concat [ rows | Right (_, _, _, _, rows) <- results ]
      byVisible = L.sortBy (\(_, a1, _) (_, b1, _) -> compare b1 a1) allRows
      top10 = P.take 10 byVisible
      bottom10 = P.take 10 (P.reverse byVisible)
  pure (object
    [ "total_parsed" .= length items
    , "num_batches" .= length batches
    , "anchor_table" .= anchorTable
    , "top10_user_visible" .= [ object ["hash" .= h, "subject" .= subjectOf h, "user_visible" .= uv, "names_component" .= nc] | (h, uv, nc) <- top10 ]
    , "bottom10_user_visible" .= [ object ["hash" .= h, "subject" .= subjectOf h, "user_visible" .= uv, "names_component" .= nc] | (h, uv, nc) <- bottom10 ]
    ])
