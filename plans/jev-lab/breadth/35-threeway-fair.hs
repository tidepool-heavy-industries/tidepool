{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
-- Was the routing failure a fact about confidence, or my own wording?
-- The original `needs_intent` alternative described a property of the repair
-- space while its three siblings described the contents of `check_output`.
-- That is the mixed-vocabulary defect measured earlier at 0.01. Here all four
-- alternatives describe contents, and nothing else changes.
fairRoute :: Member Jev effs => Text -> Text -> Eff effs Text
fairRoute oid out = do
  let q = J.choice "Which statement describes `check_output`?"
        ( J.alt #mechanical
            (String "`check_output` names a lint rule, or shows a formatting diff, and states the replacement text.")
            ("mechanical" :: Text)
          J..| J.alt #needs_writing
            (String "`check_output` reports code that does not exist yet, such as match arms that are not covered.")
            "needs_writing"
          J..| J.alt #needs_intent
            (String "`check_output` names one definition and several sites that call it, and reports that the two disagree.")
            "needs_intent"
          J..| J.alt #unreadable
            (String "`check_output` contains no compiler diagnostic naming a file and a line.")
            "unreadable" )
  answer <- J.ask1 (J.state (object ["check_output" .= out])) q
  pure (case answer of
    Left e -> oid <> " jev_error " <> T.pack (show e)
    Right a -> T.take 7 oid <> " " <> a.key
                 <> " m=" <> T.pack (show a.mass)
                 <> " c=" <> T.pack (show a.confidence))

diagOnly2 :: Text -> Text
diagOnly2 t = T.unlines (take 26
  [ l | l <- T.lines t
      , T.isPrefixOf "error" l || T.isInfixOf ".rs:" l || T.isPrefixOf "note:" l
      , not (T.isInfixOf "could not compile" l) ])

do
  let fx n = "/home/inanna/.claude/jobs/4940a626/tmp/lab5/fx/" <> n <> "-check.out"
  rs <- mapM (\n -> sh ["cat", fx n] >>= fairRoute n . diagOnly2)
             ["4610b5e", "f726882", "53ad43c"]
  pure (String (T.intercalate " | " rs))
