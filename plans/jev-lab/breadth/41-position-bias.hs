{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
import Tidepool.Effects.Core (Jev, Commands)

sh :: Member Commands effs => [Text] -> Eff effs Text
sh args = do
  r <- Cmd.run (Cmd.argv args)
  pure (either (const "") id (Cmd.stdout r))

massOf :: Text -> [(Text, Double)] -> Double
massOf k ms = case lookup k ms of
  Just v -> v
  Nothing -> -1

do
  raw <- sh ["cat", "/home/inanna/.claude/jobs/4940a626/tmp/lab5/fx/53ad43c-check.out"]
  let n = T.length raw
      text = T.drop (max 0 (n - 3000)) raw
      st = J.state (object ["text" .= text])
      descWrongArity = "`text` shows a function call site given a different number of arguments than the function's definition takes."
      descNonExhaustive = "`text` shows a `match` or case expression that fails to cover every variant of the type it matches on."
      descLintAsError = "`text` shows a compiler warning or lint that has been promoted to a hard, build-failing error."
      descAssertionFailed = "`text` shows a test whose `assert` or `assert_eq!` check failed when the test was run."
      descNoneOfThese = "`text` does not name a wrong-argument-count error, a non-exhaustive match, a promoted lint, or a failed test assertion."
      q1 = J.choice "Which statement describes the failure shown in `text`?"
        ( J.alt #wrong_arity (String descWrongArity) ()
          J..| J.alt #nonexhaustive_match (String descNonExhaustive) ()
          J..| J.alt #lint_as_error (String descLintAsError) ()
          J..| J.alt #assertion_failed (String descAssertionFailed) ()
          J..| J.alt #none_of_these (String descNoneOfThese) () )
      q2 = J.choice "Which statement describes the failure shown in `text`?"
        ( J.alt #nonexhaustive_match (String descNonExhaustive) ()
          J..| J.alt #lint_as_error (String descLintAsError) ()
          J..| J.alt #assertion_failed (String descAssertionFailed) ()
          J..| J.alt #none_of_these (String descNoneOfThese) ()
          J..| J.alt #wrong_arity (String descWrongArity) () )
      q3 = J.choice "Which statement describes the failure shown in `text`?"
        ( J.alt #lint_as_error (String descLintAsError) ()
          J..| J.alt #assertion_failed (String descAssertionFailed) ()
          J..| J.alt #none_of_these (String descNoneOfThese) ()
          J..| J.alt #wrong_arity (String descWrongArity) ()
          J..| J.alt #nonexhaustive_match (String descNonExhaustive) () )
      q4 = J.choice "Which statement describes the failure shown in `text`?"
        ( J.alt #assertion_failed (String descAssertionFailed) ()
          J..| J.alt #none_of_these (String descNoneOfThese) ()
          J..| J.alt #wrong_arity (String descWrongArity) ()
          J..| J.alt #nonexhaustive_match (String descNonExhaustive) ()
          J..| J.alt #lint_as_error (String descLintAsError) () )
      q5 = J.choice "Which statement describes the failure shown in `text`?"
        ( J.alt #none_of_these (String descNoneOfThese) ()
          J..| J.alt #wrong_arity (String descWrongArity) ()
          J..| J.alt #nonexhaustive_match (String descNonExhaustive) ()
          J..| J.alt #lint_as_error (String descLintAsError) ()
          J..| J.alt #assertion_failed (String descAssertionFailed) () )
      packet =
        #round1 := q1
          :& #round2 := q2
          :& #round3 := q3
          :& #round4 := q4
          :& #round5 := q5
          :& Nil
  answer <- J.ask st packet
  case answer of
    Left e -> pure (object ["jev_error" .= T.pack (show e)])
    Right r -> do
      let a = J.answers r
          rounds = [a.round1.masses, a.round2.masses, a.round3.masses, a.round4.masses, a.round5.masses]
          keys = ["wrong_arity", "nonexhaustive_match", "lint_as_error", "assertion_failed", "none_of_these"]
          table = [ (k, [massOf k ms | ms <- rounds]) | k <- keys ]
          spreads = [ (k, maximum vs - minimum vs) | (k, vs) <- table ]
      pure (object
        [ "round_selected_keys" .= [a.round1.key, a.round2.key, a.round3.key, a.round4.key, a.round5.key]
        , "table" .= [ object ["alt" .= k, "masses_by_round" .= vs] | (k, vs) <- table ]
        , "spreads" .= [ object ["alt" .= k, "spread" .= s] | (k, s) <- spreads ]
        ])
