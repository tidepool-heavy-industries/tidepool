{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
import Tidepool.Effects.Core (Jev, Commands)

sh :: Member Commands effs => [Text] -> Eff effs Text
sh args = do
  r <- Cmd.run (Cmd.argv args)
  pure (either (const "") id (Cmd.stdout r))

-- One step of a typed action dispatcher. The alternatives carry the command
-- that runs; code never interprets a key into an action.
dispatch :: (Member Jev effs, Member Commands effs) => Text -> Text -> [Text] -> Eff effs Value
dispatch oid assignment seen = do
  let menu =
        J.alt #enough
          (String "`observations` already says whether the change at the definition was deliberate, so no further read is needed.")
          (pure ("nothing fetched" :: Text))
        J..| J.many
          [ ( "read_commit_message"
            , String "`observations` contains no commit message for `revision`, and the message states what the author changed and why."
            , sh ["git", "log", "-1", "--format=%B", oid] )
          , ( "show_definition"
            , String "`observations` contains no diff of the changed definition, which would show whether the new parameter is used in the body."
            , T.take 1200 <$> sh ["git", "show", oid, "--", "src/store.rs"] )
          , ( "grep_callers"
            , String "`observations` contains no tree-wide list of call sites, and the compiler reports only the ones that fail to compile."
            , sh ["git", "grep", "-n", "load(", oid] )
          , ( "read_requirements"
            , String "`observations` contains no written requirement text, which would say whether a limit on the number of items was ever asked for."
            , sh ["grep", "-n", "-i", "limit", "TASKS.md"] )
          ]
      packet =
        #next := J.choice
          "Which observation is missing from `observations` and would settle whether the signature change was deliberate?"
          menu
          :& #term := J.given "the next observation is a tree-wide search for call sites"
               (J.noul "Is `load` the right term to search the tree for, rather than the name of the function that contains the first reported call site?")
          :& #settled := J.noul
               "Does `observations` already state whether the parameter added at the definition was added on purpose?"
          :& Nil
  answer <- J.ask
    (J.state (object
      [ "revision" .= oid
      , "assignment" .= assignment
      , "failed_check" .= ("four errors: this function takes 2 arguments but 1 argument was supplied" :: Text)
      , "observations" .= (if null seen then "(none yet)" else T.intercalate "\n---\n" seen)
      ]))
    packet
  case answer of
    Left e -> pure (object ["jev_error" .= T.pack (show e)])
    Right r -> do
      let a = J.answers r
      out <- J.handle a.next.chosen (#enough id J..| J.onMany (\_ act -> act))
      pure (object
        [ "key" .= a.next.key
        , "mass" .= a.next.mass
        , "margin" .= a.next.margin
        , "confidence" .= a.next.confidence
        , "masses" .= T.pack (show a.next.masses)
        , "term_is_load" .= a.term.yes
        , "already_settled" .= a.settled.yes
        , "fetched" .= T.take 700 out
        ])

do
  r <- Cmd.run (Cmd.argv ["cat", "/home/inanna/.claude/jobs/4940a626/tmp/lab5/fx/assign-46.txt"])
  let asn = T.take 900 (either (const "") id (Cmd.stdout r))
  step1 <- dispatch "53ad43c" asn []
  pure step1
