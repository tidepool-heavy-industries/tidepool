{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
-- Three-way branching. One judgment, three destinations chosen by confidence:
-- code acts, a model is briefed, or a person is asked. Each branch does real
-- read-only work, so the routing is not a label but a different program.
--
-- Three real failed checks whose correct destination is known independently:
--   4610b5e  a clippy lint promoted by -D warnings   -> code can fix this
--   f726882  five non-exhaustive match arms          -> a model must write them
--   53ad43c  wrong arity, two coherent repairs       -> only intent settles it

-- What each branch actually does, all read-only.
actCode :: Member Commands effs => Text -> Eff effs Text
actCode oid = do
  hits <- sh ["git", "grep", "-n", "Default::default()", oid]
  pure ("code acts. the mechanical site list is:\n" <> T.take 300 hits)

briefModel :: Member Commands effs => Text -> Eff effs Text
briefModel oid = do
  body <- sh ["git", "grep", "-n", "-A", "6", "match self.active", oid]
  pure ("a model is briefed with the enclosing code:\n" <> T.take 300 body)

askPerson :: Member Commands effs => Text -> Eff effs Text
askPerson oid = do
  msg <- sh ["git", "log", "-1", "--format=%s", oid]
  pure ("a person is asked, because two repairs both fit. the commit says: "
        <> T.strip msg
        <> ". which was meant, updating the callers or restoring the signature?")

routeCheck :: (Member Jev effs, Member Commands effs) => Text -> Text -> Eff effs Value
routeCheck oid out = do
  let q = J.choice "Which statement describes `check_output`?"
        ( J.alt #mechanical
            (String "`check_output` names a lint rule or shows a formatting diff, and the repair it asks for is fully determined by the diagnostic text.")
            (actCode oid)
          J..| J.alt #needs_writing
            (String "`check_output` reports missing code that has to be written, such as match arms that do not exist yet, and the diagnostic says where but not what.")
            (briefModel oid)
          J..| J.alt #needs_intent
            (String "`check_output` reports a conflict that two different repairs would both resolve, and the diagnostic text does not say which was meant.")
            (askPerson oid)
          J..| J.alt #unreadable
            (String "`check_output` contains no compiler diagnostic naming a file and a line.")
            (pure "nothing to routeCheck") )
  answer <- J.ask1 (J.state (object ["revision" .= oid, "check_output" .= out])) q
  case answer of
    Left e -> pure (String (oid <> " jev_error " <> T.pack (show e)))
    Right a -> do
      -- The three destinations, by confidence. Only the top tier acts on its own.
      let tier | a.confidence >= 0.70 = "code acts on its own" :: Text
               | a.confidence >= 0.40 = "escalate to a model"
               | otherwise = "ask a person"
      did <- J.handle a.chosen
        (#mechanical id J..| #needs_writing id J..| #needs_intent id J..| #unreadable id)
      pure (String (oid <> " | " <> a.key <> " | mass " <> T.pack (show a.mass)
              <> " | conf " <> T.pack (show a.confidence) <> " | " <> tier
              <> " | " <> T.take 60 (T.replace "\n" " " did)))

-- Only the diagnostic lines. A whole check log is mostly nix and cargo
-- preamble, and carrying it costs display budget without adding evidence.
diagLines :: Text -> Text
diagLines t = T.unlines (take 40
  [ l | l <- T.lines t
      , T.isPrefixOf "error" l || T.isPrefixOf "warning:" l
        || T.isInfixOf ".rs:" l || T.isPrefixOf "note:" l
      , not (T.isInfixOf "could not compile" l) ])

do
  let fx n = "/home/inanna/.claude/jobs/4940a626/tmp/lab5/fx/" <> n <> "-check.out"
  sh ["cat", fx "53ad43c"] >>= routeCheck "53ad43c" . diagLines
