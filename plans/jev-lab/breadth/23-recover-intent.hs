{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
-- The decisive test. Yesterday the investigation refused to choose a repair
-- because it had no statement of intent, and a human had to supply one.
-- Here code fetches evidence itself and the same choice is asked three ways:
-- with nothing, with the evidence the dispatcher gathered, and with the
-- sentence the human supplied. If the middle column matches the right-hand
-- one, the program recovered the missing fact without being told.
strategyGiven :: Member Jev effs => Text -> Text -> Text -> Eff effs Value
strategyGiven label evidence intent = do
  let q = J.choice
        "Which repair was meant? Read `evidence` and `intent` first; the diagnostics alone cannot settle this."
        ( J.alt #callers_catch_up
            (String "The code at the location the diagnostics point to as the definition was changed on purpose, and the sites the compiler reported are stale callers that have to catch up with it.")
            ("callers_catch_up" :: Text)
          J..| J.alt #restore_the_definition
            (String "The change at that definition was not meant, and putting it back is the repair; the reported sites are already correct as written.")
            "restore_the_definition"
          J..| J.alt #insufficient_evidence
            (String "Nothing in `evidence` or `intent` says which of those two repairs was meant.")
            "insufficient_evidence" )
  answer <- J.ask1
    (J.state (object
      [ "failed_check" .= ("four errors E0061: this function takes 2 arguments but 1 argument was supplied, at src/main.rs:83 and src/store.rs:81,100,110; note: function defined here src/store.rs:59" :: Text)
      , "evidence" .= evidence
      , "intent" .= intent
      ]))
    q
  case answer of
    Left e -> pure (object ["case" .= label, "jev_error" .= T.pack (show e)])
    Right a -> pure (object
      [ "case" .= label
      , "key" .= a.key
      , "mass" .= a.mass
      , "margin" .= a.margin
      , "confidence" .= a.confidence
      , "accepted" .= either (\d -> "doubt: " <> T.pack (show d)) J.selectedKey (J.accept J.merging a)
      , "masses" .= T.pack (show a.masses)
      ])

do
  msg <- sh ["git", "log", "-1", "--format=%B", "53ad43c"]
  dif <- T.take 900 <$> sh ["git", "show", "53ad43c", "--", "src/store.rs"]
  cal <- sh ["git", "grep", "-n", "load(", "53ad43c"]
  let gathered = T.intercalate "\n---\n"
        [ "commit message:\n" <> msg, "definition diff:\n" <> dif, "call sites:\n" <> cal ]
  blank <- strategyGiven "no evidence, no intent" "(none)" "(none)"
  found <- strategyGiven "evidence the program fetched itself" gathered "(none)"
  told <- strategyGiven "the sentence a human supplied" "(none)"
            "the commit deliberately adds a limit parameter to store::load"
  pure (object ["evidence_bytes" .= T.length gathered, "cases" .= [blank, found, told]])
