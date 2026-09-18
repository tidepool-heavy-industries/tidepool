{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
-- Why did the dispatcher never stop? Four forms of the same stop question,
-- one request, one identical state that already contains everything needed.
--   full     : the original menu, completion written with concrete vocabulary
--   pruned   : code removed the three actions whose artifacts are already present
--   separate : the same concrete question asked as its own noul
--   vague    : the original wording, as a noul, for comparison
settledProbe :: Member Jev effs => Text -> Eff effs Value
settledProbe obs = do
  let -- the vocabulary that actually settles it: name the artifacts, not the belief
      done = String "`observations` already contains both a commit message naming the new parameter and a diff in which the body of the changed function uses that parameter."
      fetchCommit = J.alt #read_commit_message (String "`observations` contains no commit message for this revision.") ("read_commit_message" :: Text)
      fetchDef = J.alt #show_definition (String "`observations` contains no diff of the changed definition.") "show_definition"
      fetchCallers = J.alt #grep_callers (String "`observations` contains no tree-wide list of call sites.") "grep_callers"
      fetchReqs = J.alt #read_requirements (String "`observations` contains no written requirement text.") "read_requirements"
      complete = J.alt #complete done "complete"
      packet =
        #full := J.choice "Which statement describes `observations`?"
          (fetchCommit J..| fetchDef J..| fetchCallers J..| fetchReqs J..| complete)
          :& #pruned := J.choice "Which statement describes `observations`?"
          (fetchReqs J..| complete)
          :& #separate := J.noul
          "Does `observations` contain both a commit message naming the new parameter and a diff in which the body of the changed function uses that parameter?"
          :& #vague := J.noul
          "Does `observations` already state whether the parameter added at the definition was added on purpose?"
          :& Nil
  answer <- J.ask (J.state (object ["observations" .= obs])) packet
  case answer of
    Left e -> pure (object ["jev_error" .= T.pack (show e)])
    Right r -> do
      let a = J.answers r
      pure (object
        [ "full_key" .= a.full.key, "full_masses" .= T.pack (show a.full.masses), "full_conf" .= a.full.confidence
        , "pruned_key" .= a.pruned.key, "pruned_masses" .= T.pack (show a.pruned.masses), "pruned_conf" .= a.pruned.confidence
        , "separate_concrete_yes" .= a.separate.yes
        , "vague_yes" .= a.vague.yes
        ])

do
  msg <- sh ["git", "log", "-1", "--format=%B", "7a48345d61ee13f2a803547ae5c05040dc7ae37d"]
  dif <- T.take 900 <$> sh ["git", "show", "7a48345d61ee13f2a803547ae5c05040dc7ae37d", "--", "tidepool-actor/src/request/updates.rs"]
  cal <- sh ["git", "grep", "-n", "update_request(", "7a48345d61ee13f2a803547ae5c05040dc7ae37d"]
  settledProbe (T.intercalate "\n---\n"
    [ "commit message:\n" <> msg, "definition diff:\n" <> dif, "call sites:\n" <> cal ])
