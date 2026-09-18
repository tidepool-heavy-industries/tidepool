{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
-- Three steps of the dispatcher, each one reading what the last one fetched.
-- `dispatch` returns the key and the text it fetched; code threads the text
-- back into `observations` and asks again. No model turn between steps.
runSteps :: (Member Jev effs, Member Commands effs) => Text -> Text -> Int -> [Text] -> [Value] -> Eff effs Value
runSteps _ _ 0 seen acc = pure (object ["steps" .= reverse acc, "observations_kept" .= length seen])
runSteps oid asn n seen acc = do
  v <- dispatch oid asn seen
  let got = case v of
        Object km -> case KM.lookup "fetched" km of { Just (String s) -> s ; _ -> "" }
        _ -> ""
      k = case v of
        Object km -> case KM.lookup "key" km of { Just (String s) -> s ; _ -> "" }
        _ -> ""
  if k == "enough" || T.null got
    then pure (object ["steps" .= reverse (v : acc), "stopped_at" .= k])
    else runSteps oid asn (n - 1) (seen <> [k <> ":\n" <> got]) (v : acc)

do
  r <- Cmd.run (Cmd.argv ["cat", "/home/inanna/.claude/jobs/4940a626/tmp/lab5/fx/assign-46.txt"])
  let asn = T.take 900 (either (const "") id (Cmd.stdout r))
  runSteps "53ad43c" asn (3 :: Int) [] []
