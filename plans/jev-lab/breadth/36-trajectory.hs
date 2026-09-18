{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
-- Trajectory-aware assistance. The claim under test is that neither a raw
-- transcript nor code-derived counters alone suffice, and that both together
-- are what let a judgment see whether attempts made progress.
-- So the same four questions run against three states: sequence only,
-- counters only, and both.

-- Everything code can derive from the trajectory without judgment.
derive :: Text -> Text
derive t =
  let acts = [ T.strip (T.drop 10 l) | l <- T.lines t, "  action: " `T.isPrefixOf` l ]
      uniq = nubOrdText acts
      exits = [ T.takeWhile (\c -> c >= '0' && c <= '9') (T.drop 15 seg)
              | seg <- T.splitOn "CommandExited " t, not (T.null seg) ]
      codes = [ T.takeWhile (\c -> c >= '0' && c <= '9') seg
              | seg <- drop 1 (T.splitOn "CommandExited " t) ]
      dup = length acts - length uniq
  in T.intercalate "\n"
       [ "attempts: " <> tshowInt (length acts)
       , "distinct commands: " <> tshowInt (length uniq)
       , "commands run more than once: " <> tshowInt dup
       , "exit codes in order: " <> T.intercalate ", " codes
       , "first timestamp: " <> firstStamp t
       , "last timestamp: " <> lastStamp t
       , "unused: " <> tshowInt (length exits)
       ]

nubOrdText :: [Text] -> [Text]
nubOrdText = go []
  where go seen [] = reverse seen
        go seen (x : xs) = if x `elem` seen then go seen xs else go (x : seen) xs

tshowInt :: Int -> Text
tshowInt = T.pack . show

stamps :: Text -> [Text]
stamps t = [ T.take 8 (T.drop 3 (snd (T.breakOn " at " l))) | l <- T.lines t, "step " `T.isPrefixOf` l ]

firstStamp :: Text -> Text
firstStamp t = case stamps t of { (s : _) -> s ; _ -> "?" }

lastStamp :: Text -> Text
lastStamp t = case reverse (stamps t) of { (s : _) -> s ; _ -> "?" }

-- Four independent conditions, so four nouls, not one choice.
probeTrajectory :: Member Jev effs => Text -> Text -> Text -> Eff effs Text
probeTrajectory label seqText derived = do
  let packet =
        #new_evidence := J.noul
          "Does any later attempt in `trajectory` run a command that returns information no earlier attempt had already returned?"
          :& #different_explanation := J.noul
          "Does any later attempt in `trajectory` test a different explanation for the failure than the attempt before it did?"
          :& #changed_candidate := J.noul
          "Do the files under test change between attempts in `trajectory`, so that later attempts run against different source than earlier ones?"
          :& #repeats_unaddressed := J.noul
          "Does the last attempt in `trajectory` run a command an earlier attempt already ran, without anything in between that addressed why it failed?"
          :& Nil
  answer <- J.ask
    (J.state (object
      [ "trajectory" .= seqText
      , "derived_facts" .= derived
      , "worker_task" .= ("implement item tags in src/app.rs, with named tests, and get check.sh green" :: Text)
      ]))
    packet
  pure (case answer of
    Left e -> label <> " jev_error " <> T.pack (show e)
    Right r ->
      let a = J.answers r
          p x = T.pack (show (fromIntegral (round (x * 100) :: Int) / 100 :: Double))
      in label <> " new=" <> p a.new_evidence.yes
           <> " diff_expl=" <> p a.different_explanation.yes
           <> " changed=" <> p a.changed_candidate.yes
           <> " repeats=" <> p a.repeats_unaddressed.yes)

do
  traj <- sh ["cat", "/home/inanna/.claude/jobs/4940a626/tmp/lab5/fx/trajectory.txt"]
  let d = derive traj
  a <- probeTrajectory "sequence_only " traj "(not supplied)"
  b <- probeTrajectory "counters_only " "(not supplied)" d
  c <- probeTrajectory "both          " traj d
  pure (String (T.intercalate "\n" [a, b, c, "", "derived was:", d]))
