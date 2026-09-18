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

-- Wordings to judge. The lab measured these: the first two separated from the
-- last two at 0.20 against 0.45 on "states the deciding fact", which matched
-- how they actually behaved when asked for real. Replace them with whatever
-- you are about to send.
lintQuestions
  [ ("vague_sufficient", "Is the evidence sufficient?")
  , ("vague_correct", "Does this look correct?")
  , ("narrow_names_file", "Does the state field `check_output` name a file that the diff in `candidate_diff` does not change?")
  , ("narrow_names_reason", "Does the state field `check_output` give a reason for the failure, rather than only the location of it?")
  ]
