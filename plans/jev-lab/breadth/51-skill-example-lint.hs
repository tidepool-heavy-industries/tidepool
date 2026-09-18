{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
import Tidepool.Effects.Core (Jev, Commands)

sh :: Member Commands effs => [Text] -> Eff effs Text
sh args = do
  r <- Cmd.run (Cmd.argv args)
  pure (either (const "") id (Cmd.stdout r))

parseBlocks :: Text -> [(Text, Text)]
parseBlocks txt =
  [ (name, T.strip body)
  | chunk <- T.splitOn "===BLOCK:" txt
  , not (T.null (T.strip chunk))
  , let (namePart, rest0) = T.breakOn "===\n" chunk
  , let name = namePart
  , let body = T.drop 4 rest0
  ]

shippedNames :: [Text]
shippedNames =
  [ "J.ask", "J.ask1", "J.choice", "J.noul", "J.score", "J.pool", "J.eachIn"
  , "J.askAbout", "J.given", "J.alt", "J..|", "J.many", "J.manyFrom", "J.accept"
  , "J.explain", "J.handle", "J.contenders", "J.selectedKey", "J.state", "J.answers"
  , "J.resolvedModel", "J.routing", "J.spawning", "J.merging"
  , "Cmd.run", "Cmd.argv", "Cmd.stdout", "R.start", "R.client", "R.call"
  , "unfold", "respond"
  ]

lintBlocks :: Member Jev effs => [(Text, Text)] -> Eff effs Value
lintBlocks blocks = do
  let pool = J.pool #blocks [ (k, String b, k) | (k, b) <- blocks ]
      packet =
        #blocks := pool
          :& #each := J.eachIn pool (\ref ->
               #only_shipped := J.askAbout ref "Does this example use only names that appear in `shipped_names`?"
                 :& #has_unshipped := J.askAbout ref "Does this example refer to a type or function by a name that `shipped_names` does not contain?"
                 :& Nil)
          :& Nil
  answer <- J.ask (J.state (object ["shipped_names" .= shippedNames])) packet
  case answer of
    Left e -> pure (object ["jev_error" .= T.pack (show e)])
    Right r -> do
      let a = J.answers r
      pure (object
        [ "rows" .= [ object ["key" .= k, "only_shipped" .= p.only_shipped.yes, "has_unshipped" .= p.has_unshipped.yes]
                    | (k, p) <- a.each ]
        ])

sh ["cat", "/home/inanna/.claude/jobs/4940a626/tmp/lab7/exp51-pool-small.txt"] >>= lintBlocks . parseBlocks
