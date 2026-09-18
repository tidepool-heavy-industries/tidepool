{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
import Tidepool.Effects.Core (Jev, Commands)

sh :: Member Commands effs => [Text] -> Eff effs Text
sh args = do
  r <- Cmd.run (Cmd.argv args)
  pure (either (const "") id (Cmd.stdout r))

do
  raw <- sh ["tail", "-n", "70", "/home/inanna/.claude/jobs/4940a626/tmp/lab5/fx/53ad43c-check.out"]
  let failText = T.take 3000 raw

  storeRaw <- T.take 1200 <$> sh ["bash", "-c", "git show 53ad43c:src/store.rs | head -c 1200"]
  mainRaw <- T.take 1200 <$> sh ["bash", "-c", "git show 53ad43c:src/main.rs | head -c 1200"]
  let storeShort = T.take 200 storeRaw
      mainShort = T.take 200 mainRaw
      storeLong = storeRaw
      mainLong = mainRaw

  let descWrongArity1 = "`text` shows a function call site given a different number of arguments than the function's definition takes."
      descNonExhaustive1 = "`text` shows a `match` or case expression that fails to cover every variant of the type it matches on."
      descLintAsError1 = "`text` shows a compiler warning or lint that has been promoted to a hard, build-failing error."
      descAssertionFailed1 = "`text` shows a test whose `assert` or `assert_eq!` check failed when the test was run."
      descNoneOfThese1 = "`text` does not name a wrong-argument-count error, a non-exhaustive match, a promoted lint, or a failed test assertion."

      descWrongArity3 = "`text` shows a function call site given a different number of arguments than the function's definition takes.\nThis happens when a signature changes (a parameter is added or removed) but a caller elsewhere in the codebase is not updated to match.\nThe compiler reports it as an argument-count mismatch tied to both the call site and the definition."
      descNonExhaustive3 = "`text` shows a `match` or case expression that fails to cover every variant of the type it matches on.\nThis happens when a type gains a new variant, or the match was written to only handle the cases its author had in mind.\nThe compiler reports it as a missing-pattern or non-exhaustive-match error tied to the match arms."
      descLintAsError3 = "`text` shows a compiler warning or lint that has been promoted to a hard, build-failing error.\nThis happens when project configuration, such as a deny attribute or a CI flag, turns an advisory warning into a build blocker.\nThe compiler reports it with the same diagnostic text a warning would use, but the build still fails."
      descAssertionFailed3 = "`text` shows a test whose `assert` or `assert_eq!` check failed when the test was run.\nThis happens when the code under test produces a different value than the test expected, discovered at runtime rather than compile time.\nThe test harness reports it as a failed assertion inside a named test function."
      descNoneOfThese3 = "`text` does not name a wrong-argument-count error, a non-exhaustive match, a promoted lint, or a failed test assertion.\nThis is the case when the failure shown is some other kind of problem, such as a linker error, a missing dependency, or an I/O failure unrelated to these four.\nNone of the specific diagnostic signatures listed above appear anywhere in `text`."

      choiceShort = J.choice "Which statement describes the failure shown in `text`?"
        ( J.alt #wrong_arity (String descWrongArity1) ()
          J..| J.alt #nonexhaustive_match (String descNonExhaustive1) ()
          J..| J.alt #lint_as_error (String descLintAsError1) ()
          J..| J.alt #assertion_failed (String descAssertionFailed1) ()
          J..| J.alt #none_of_these (String descNoneOfThese1) () )
      choiceLong = J.choice "Which statement describes the failure shown in `text`?"
        ( J.alt #wrong_arity (String descWrongArity3) ()
          J..| J.alt #nonexhaustive_match (String descNonExhaustive3) ()
          J..| J.alt #lint_as_error (String descLintAsError3) ()
          J..| J.alt #assertion_failed (String descAssertionFailed3) ()
          J..| J.alt #none_of_these (String descNoneOfThese3) () )

      poolShort = J.pool #short_items [("store", String storeShort, "store" :: Text), ("main", String mainShort, "main")]
      poolLong = J.pool #long_items [("store", String storeLong, "store" :: Text), ("main", String mainLong, "main")]
      missingFileQ = "Does this text document or implement handling for a missing or nonexistent file?"

      packet =
        #short_choice := choiceShort
          :& #long_choice := choiceLong
          :& #short_items := poolShort
          :& #short_each := J.eachIn poolShort (\ref -> #handles := J.askAbout ref missingFileQ :& Nil)
          :& #long_items := poolLong
          :& #long_each := J.eachIn poolLong (\ref -> #handles := J.askAbout ref missingFileQ :& Nil)
          :& Nil

  answer <- J.ask (J.state (object ["text" .= failText])) packet
  case answer of
    Left e -> pure (object ["jev_error" .= T.pack (show e)])
    Right r -> do
      let a = J.answers r
          getYes ks k = case lookup k ks of
            Just sub -> sub.handles.yes
            Nothing -> -1
      pure (object
        [ "choice_short" .= object ["key" .= a.short_choice.key, "mass" .= a.short_choice.mass, "masses" .= a.short_choice.masses]
        , "choice_long" .= object ["key" .= a.long_choice.key, "mass" .= a.long_choice.mass, "masses" .= a.long_choice.masses]
        , "pool_short_store_handles" .= getYes a.short_each "store"
        , "pool_short_main_handles" .= getYes a.short_each "main"
        , "pool_long_store_handles" .= getYes a.long_each "store"
        , "pool_long_main_handles" .= getYes a.long_each "main"
        ])
