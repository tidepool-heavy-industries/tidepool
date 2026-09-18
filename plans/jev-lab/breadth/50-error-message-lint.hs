{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
import Tidepool.Effects.Core (Jev, Commands)

sh :: Member Commands effs => [Text] -> Eff effs Text
sh args = do
  r <- Cmd.run (Cmd.argv args)
  pure (either (const "") id (Cmd.stdout r))

parseMessages :: Text -> [(Text, Text)]
parseMessages txt =
  [ (k, m)
  | line <- T.lines txt
  , not (T.null line)
  , let parts = T.splitOn "\t" line
  , length parts == 2
  , let [k, m] = parts
  ]

lintMessages :: Member Jev effs => [(Text, Text)] -> Eff effs Value
lintMessages items = do
  let pool = J.pool #messages [ (k, String m, k) | (k, m) <- items ]
      packet =
        #messages := pool
          :& #each := J.eachIn pool (\ref ->
               #specific := J.askAbout ref "Does this message name the specific thing that was refused or that failed, rather than only the category of failure?"
                 :& #actionable := J.askAbout ref "Does this message name a concrete action, directory, value or alternative that would have worked instead?"
                 :& #cause := J.askAbout ref "Does this message name the state or condition that caused the failure, such as what the actor holds or where it is?"
                 :& Nil)
          :& Nil
  answer <- J.ask (J.state (object ["task" .= ("judging whether an engine error message names the refused thing, an alternative, and the cause" :: Text)])) packet
  case answer of
    Left e -> pure (object ["jev_error" .= T.pack (show e)])
    Right r -> do
      let a = J.answers r
      pure (object
        [ "rows" .= [ object ["key" .= k, "specific" .= p.specific.yes, "actionable" .= p.actionable.yes, "cause" .= p.cause.yes]
                    | (k, p) <- a.each ]
        ])

do
  txt <- sh ["cat", "/home/inanna/.claude/jobs/4940a626/tmp/lab7/exp50-messages.txt"]
  lintMessages (parseMessages txt)
