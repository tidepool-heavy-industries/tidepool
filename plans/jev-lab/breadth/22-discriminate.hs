{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
-- Which observation would best tell two explanations apart?
-- For each candidate observation, the same concrete question is asked twice,
-- once under each explanation as a premise. The gap between the two answers is
-- the observation's discriminating power, computed in code.
discriminate :: Member Jev effs => Text -> Eff effs Value
discriminate failure = do
  let deliberate = "The parameter added to the definition of `load` was added on purpose, and the callers are stale."
      accident = "The parameter added to the definition of `load` was not meant to be there, and the callers are correct as written."
      packet =
        #commit_del := J.given deliberate
          (J.noul "Would the commit message for this revision name the new parameter or the behaviour it adds?")
          :& #commit_acc := J.given accident
          (J.noul "Would the commit message for this revision name the new parameter or the behaviour it adds?")
          :& #body_del := J.given deliberate
          (J.noul "Would the body of the changed function use the new parameter to do something?")
          :& #body_acc := J.given accident
          (J.noul "Would the body of the changed function use the new parameter to do something?")
          :& #callers_del := J.given deliberate
          (J.noul "Would every call site outside the definition still pass the old number of arguments?")
          :& #callers_acc := J.given accident
          (J.noul "Would every call site outside the definition still pass the old number of arguments?")
          :& #reqs_del := J.given deliberate
          (J.noul "Would the written requirements in `requirements_file` mention a limit on the number of items loaded?")
          :& #reqs_acc := J.given accident
          (J.noul "Would the written requirements in `requirements_file` mention a limit on the number of items loaded?")
          :& Nil
  answer <- J.ask
    (J.state (object
      [ "failed_check" .= failure
      , "changed_definition" .= ("src/store.rs:59 pub fn load(path: impl AsRef<Path>, limit: usize)" :: Text)
      , "requirements_file" .= ("TASKS.md, the written goal for this feature" :: Text)
      ]))
    packet
  case answer of
    Left e -> pure (object ["jev_error" .= T.pack (show e)])
    Right r -> do
      let a = J.answers r
          rows = [ ("read_commit_message" :: Text, a.commit_del.yes, a.commit_acc.yes)
                 , ("show_definition", a.body_del.yes, a.body_acc.yes)
                 , ("grep_callers", a.callers_del.yes, a.callers_acc.yes)
                 , ("read_requirements", a.reqs_del.yes, a.reqs_acc.yes) ]
          gap (k, d, c) = (k, d, c, abs (d - c))
          scored = map gap rows
          best = L.maximumBy (\(_,_,_,x) (_,_,_,y) -> compare x y) scored
      pure (object
        [ "rows" .= [ object ["observation" .= k, "if_deliberate" .= d, "if_accident" .= c, "separates_by" .= g]
                    | (k, d, c, g) <- scored ]
        , "most_discriminating" .= (\(k,_,_,g) -> object ["observation" .= k, "separates_by" .= g]) best
        ])

do
  r <- Cmd.run (Cmd.argv ["cat", "/home/inanna/.claude/jobs/4940a626/tmp/lab5/fx/53ad43c-check.out"])
  let chk = T.take 1400 (either (const "") id (Cmd.stdout r))
  discriminate chk
