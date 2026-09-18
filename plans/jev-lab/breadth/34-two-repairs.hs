{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
-- Routing by confidence called the ambiguous failure mechanical at 0.81.
-- So ask the structural fact directly instead of hoping confidence reveals it.
-- Two literal questions per failure, each about one whole repair; code decides
-- that a person is needed when both repairs would work. This is atomic
-- decomposition: neither question asks whether the case is ambiguous.
twoRepairs :: Member Jev effs => [(Text, Text)] -> Eff effs Value
twoRepairs fixtures = do
  let pool = J.pool #failures [ (k, String v, k) | (k, v) <- fixtures ]
      packet =
        #failures := pool
          :& #each := J.eachIn pool (\ref ->
               #revert_fixes := J.askAbout ref
                 "Would putting back the code at the location these diagnostics name as the definition make every reported error go away, without editing any of the reported sites?"
                 :& #edit_sites_fixes := J.askAbout ref
                      "Would editing every site these diagnostics report make every error go away, without changing the code at the location they name as the definition?"
                 :& Nil)
          :& Nil
  answer <- J.ask (J.state (object ["note" .= ("each failure is the diagnostic lines of one failed build" :: Text)])) packet
  case answer of
    Left e -> pure (String ("jev_error " <> T.pack (show e)))
    Right r -> do
      let a = J.answers r
          two x = T.pack (show (fromIntegral (round (x * 100) :: Int) / 100 :: Double))
          row (k, p) = T.take 7 k <> " r=" <> two p.revert_fixes.yes <> " e=" <> two p.edit_sites_fixes.yes
      pure (String (T.intercalate "  " (map row a.each)))

diagOnly :: Text -> Text
diagOnly t = T.unlines (take 30
  [ l | l <- T.lines t
      , T.isPrefixOf "error" l || T.isInfixOf ".rs:" l || T.isPrefixOf "note:" l
      , not (T.isInfixOf "could not compile" l) ])

do
  let fx n = "/home/inanna/.claude/jobs/4940a626/tmp/lab5/fx/" <> n <> "-check.out"
  fs <- mapM (\n -> (,) n . diagOnly <$> sh ["cat", fx n]) ["4610b5e", "f726882", "53ad43c"]
  twoRepairs fs
