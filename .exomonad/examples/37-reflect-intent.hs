-- Contextual investigation with simulated Reflect output.
--
-- Real matched fixture, all three pieces from the same past run:
--   task_instructions    the assignment actually sent to that worker
--   conversation_history its own 17 real steps, ending at commit f726882
--   repository_evidence  the check that failed at f726882
--
-- The assignment says "Own only src/app.rs; change no other file". The
-- failure names src/main.rs and src/panels/status.rs. So whether this red
-- build is the worker's problem is answerable from its own history and not
-- from the diagnostics. Three fields, kept separate and named.
whoRepairs :: Member Jev effs => Text -> Text -> Text -> Text -> Text -> Eff effs Text
whoRepairs label instructions history correction evidence = do
  let q = J.choice "Which statement describes these materials?"
        -- All three name the same authority in the same words: the latest
        -- instruction, wherever it appears. Naming one field as the authority
        -- made a later correction unable to win by construction.
        ( J.alt #worker_incomplete
            (String "Reading the latest instruction, in `task_instructions` or in `latest_correction`, or in a later correction inside `conversation_history`, `repository_evidence` names an error in a file that instruction assigns to this worker.")
            ("worker_incomplete" :: Text)
          J..| J.alt #expected_handoff
            (String "Reading the latest instruction, in `task_instructions` or in `latest_correction`, or in a later correction inside `conversation_history`, `repository_evidence` names errors only in files that instruction does not assign to this worker, and `conversation_history` shows the required command was run and the work committed.")
            "expected_handoff"
          J..| J.alt #unresolved
            (String "Reading the latest instruction, in `task_instructions` or in `latest_correction`, or in a later correction inside `conversation_history`, no instruction names the files this worker owns, or `conversation_history` does not show whether the required command was run.")
            "unresolved" )
  answer <- J.ask1
    (J.state (object
      [ "task_instructions" .= instructions
      , "conversation_history" .= history
      , "latest_correction" .= correction
      , "repository_evidence" .= evidence
      ]))
    q
  pure (case answer of
    Left e -> label <> " jev_error " <> T.pack (show e)
    Right a -> label <> " " <> a.key
                 <> " m=" <> T.pack (show a.mass)
                 <> " c=" <> T.pack (show a.confidence))

diagsOnly :: Text -> Text
diagsOnly t = T.unlines (take 24
  [ l | l <- T.lines t
      , T.isPrefixOf "error" l || T.isInfixOf ".rs:" l || T.isPrefixOf "note:" l
      , not (T.isInfixOf "could not compile" l) ])

do
  let fxp n = ".shoal/examples/fixtures/" <> n
  asn <- sh ["cat", fxp "assign-14.txt"]
  hist <- sh ["cat", fxp "trajectory.txt"]
  chk <- diagsOnly <$> sh ["cat", fxp "f726882-check.out"]
  -- A later correction that overrides the original ownership. Constructed, and
  -- marked as such: it is the one piece here that is not from the real run.
  let superseded = hist <> "\nstep 18 at 20:41:02\n  action: message from the parent (later than every step above)\n  outcome: correction: you now also own src/panels/status.rs and src/main.rs. Add the missing match arms in those files before you reply.\n"
  d <- whoRepairs "superseded, buried  " asn superseded "(none)" chk
  f <- whoRepairs "superseded, hoisted " asn hist "you now also own src/panels/status.rs and src/main.rs. Add the missing match arms in those files before you reply." chk
  pure (String (T.intercalate "\n" [d, f]))
