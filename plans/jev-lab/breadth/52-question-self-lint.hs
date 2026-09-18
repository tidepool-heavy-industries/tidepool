{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
import Tidepool.Effects.Core (Jev, Commands)

sh :: Member Commands effs => [Text] -> Eff effs Text
sh args = do
  r <- Cmd.run (Cmd.argv args)
  pure (either (const "") id (Cmd.stdout r))

parseQuestions :: Text -> [(Text, Text)]
parseQuestions txt =
  [ (k, q)
  | line <- T.lines txt
  , not (T.null line)
  , let parts = T.splitOn "\t" line
  , length parts == 2
  , let [k, q] = parts
  ]

lintQuestions :: Member Jev effs => [(Text, Text)] -> Eff effs Value
lintQuestions items = do
  let pool = J.pool #questions [ (k, String q, k) | (k, q) <- items ]
      packet =
        #questions := pool
          :& #each := J.eachIn pool (\ref ->
               #names_field := J.askAbout ref "Does this question name a field of the state it will be asked against, in backticks?"
                 :& #states_deciding_fact := J.askAbout ref "Does this question state the specific fact that decides the answer, rather than asking for an overall judgment?"
                 :& #gives_why := J.askAbout ref "Does this question say why the condition would hold, giving something concrete to check?"
                 :& Nil)
          :& Nil
  answer <- J.ask (J.state (object ["task" .= ("judging whether a candidate question about a repair is well-formed" :: Text)])) packet
  case answer of
    Left e -> pure (object ["jev_error" .= T.pack (show e)])
    Right r -> do
      let a = J.answers r
      pure (object
        [ "rows" .= [ object ["key" .= k, "names_field" .= p.names_field.yes, "states_deciding_fact" .= p.states_deciding_fact.yes, "gives_why" .= p.gives_why.yes]
                    | (k, p) <- a.each ]
        ])

sh ["cat", "/home/inanna/.claude/jobs/4940a626/tmp/lab7/exp52-questions.txt"] >>= lintQuestions . parseQuestions
