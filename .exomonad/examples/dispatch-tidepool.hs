-- A typed action dispatcher, asked about a real change in this repository.
--
-- Commit 7a48345d6 changed `update_request` so that a success carries its
-- delivery instead of an `Option`. A caller that had not been updated would see
-- a type error at the call site, and the question a reader then has is whether
-- the signature moved on purpose or by accident. That is the same question the
-- original experiment asked of a demo repository, against artifacts that
-- actually exist here.
--
-- What makes it a dispatcher rather than a menu: each alternative carries the
-- command that answers it. The model chooses; the cell runs the choice; no model
-- turn happens in between. Each alternative also names what `observations`
-- currently lacks, so satisfying one removes its own reason to be chosen — which
-- is why a loop built from this does not repeat itself.
--
-- The original and its recorded numbers are in plans/jev-lab/breadth/10-dispatch.hs
-- and 11-dispatch-loop.hs. Those are kept as they ran; this is the invocation to
-- copy from.
import Tidepool.Effects.Core (Jev, Commands)

sh :: Member Commands effs => [Text] -> Eff effs Text
sh args = do
  r <- Cmd.quiet (Cmd.run (Cmd.argv args))
  pure (either (const "") id (Cmd.stdout r))

dispatch :: (Member Jev effs, Member Commands effs) => Text -> Text -> [Text] -> Eff effs Value
dispatch oid assignment seen = do
  let menu =
        J.alt #enough
          (String "`observations` already says whether the signature change at the definition was deliberate, so no further read is needed.")
          (pure ("nothing fetched" :: Text))
        J..| J.many
          [ ( "read_commit_message"
            , String "`observations` contains no commit message for `revision`, and the message states what the author changed and why."
            , sh ["git", "log", "-1", "--format=%B", oid] )
          , ( "show_definition"
            , String "`observations` contains no diff of the changed definition, which would show whether the new return type is used in the body."
            , T.take 1200 <$> sh ["git", "show", oid, "--", "exomonad/actor/src/request/updates.rs"] )
          , ( "grep_callers"
            , String "`observations` contains no tree-wide list of call sites, and the compiler reports only the ones that fail to compile."
            , sh ["git", "grep", "-n", "update_request(", oid] )
          , ( "read_findings"
            , String "`observations` contains no written record of the defect, which would say whether this change was asked for."
            , sh ["grep", "-n", "-i", "updateRequest", "plans/jev-lab/EVAL-RUNS.md"] )
          ]
      packet =
        #next := J.choice
          "Which observation is missing from `observations` and would settle whether the signature change was deliberate?"
          menu
          :& #term := J.given "the next observation is a tree-wide search for call sites"
               (J.noul "Is `update_request(` the right term to search the tree for, rather than the name of the function that contains the first reported call site?")
          :& #settled := J.noul
               "Does `observations` already state whether the return type changed at the definition was changed on purpose?"
          :& Nil
  answer <- J.ask
    (J.state (object
      [ "revision" .= oid
      , "assignment" .= assignment
      , "failed_check" .= ("error[E0308]: mismatched types: expected `Option<RequestUpdateDelivery>`, found `RequestUpdateDelivery`" :: Text)
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
        , "term_is_right" .= a.term.yes
        , "already_settled" .= a.settled.yes
          -- Known defect, left in so it is visible: what gets appended to
          -- `observations` on the next step is this truncation, not the whole
          -- fetch, and the deciding line can fall past 700 characters. Fix this
          -- first if you build a loop on it.
        , "fetched" .= T.take 700 out
        ])

do
  -- One sentence of intent. The strongest single result in the original survey
  -- was that supplying it moved a refusal to a confident answer in both
  -- directions, with no threshold change.
  let assignment = "Refuse an update the target can no longer be shown, rather than \
                   \accepting it and reporting success. A caller that reads a success \
                   \must be able to act on it."
  dispatch "7a48345d61ee13f2a803547ae5c05040dc7ae37d" assignment []
