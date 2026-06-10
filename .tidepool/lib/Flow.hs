{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
-- | Stateful control-flow over continuations: sentinels (heap-held
-- snapshots diffed on demand), sagas (KV-checkpointed resumable
-- workflows), and escalation (suspend only for genuine ambiguity).
module Flow where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import Asks
import qualified Tidepool.Aeson.KeyMap as KM

-- | Compact one-line rendering of a Value leaf (for diffs/prompts).
vshow :: Value -> Text
vshow v = case v of
  String t -> t
  Bool b -> if b then "true" else "false"
  Null -> "null"
  Array xs -> "[" <> pack (show (len xs)) <> " items]"
  Object m -> "{" <> pack (show (len (KM.toList m))) <> " fields}"
  _ -> case v ^? _Number of
    Just n -> pack (show (round n :: Int))
    Nothing -> "?"

-- | Structural diff: "path: old -> new" lines. Objects diff key-wise,
-- everything else compares whole (Ord Value gives us Eq).
vdiff :: Value -> Value -> [Text]
vdiff = go ""
  where
    go path a b =
      if a == b
        then []
        else case (a, b) of
          (Object x, Object y) ->
            let keys = nub (map (KM.toText . fst) (KM.toList x) <> map (KM.toText . fst) (KM.toList y))
                look k m = case lookupKey k (Object m) of { Just v -> v; Nothing -> Null }
            in concatMap (\k -> go (path <> "/" <> k) (look k x) (look k y)) keys
          _ -> [path <> ": " <> vshow a <> " -> " <> vshow b]

-- | A sentinel coroutine: scan once, hold the snapshot IN THE HEAP,
-- and on each wake-up rescan + report exactly what changed. Each
-- suspension carries the previous round's diff in its prompt. The
-- caller replies 'check' to rescan or 'stop' to finish (returning the
-- final snapshot). Runs indefinitely — every resume gets a fresh
-- eval-timeout window.
watch :: Text -> M Value -> M Value
watch label scan = do
  s0 <- scan
  go (1 :: Int) s0 "watch started; snapshot held"
  where
    go n prev note = do
      cmd <- oracle ("[watch:" <> label <> " #" <> pack (show n) <> "] " <> note <> " | reply check/stop")
      if toLower (strip cmd) == "stop"
        then pure prev
        else do
          cur <- scan
          let d = vdiff prev cur
          go (n + 1) cur (if isNull d then "no changes" else "CHANGED:\n" <> intercalate "\n" d)

-- | Crash-resumable workflow: each step's result checkpoints to KV.
-- If a later step dies (error, timeout, lost continuation), re-running
-- the SAME saga skips completed steps instantly. `sagaReset` clears.
saga :: Text -> [(Text, M Value)] -> M Value
saga name steps = do
  results <- mapM runStep steps
  pure (object (map (\(k, v) -> k .= v) results))
  where
    key step = "saga:" <> name <> ":" <> step
    runStep (step, act) = do
      cached <- kvGet (key step)
      case cached of
        Just v -> pure (step, v)
        Nothing -> do
          v <- act
          kvSet (key step) v
          pure (step, v)

-- | Forget a saga's checkpoints.
sagaReset :: Text -> M ()
sagaReset name = do
  ks <- kvKeys
  mapM_ (\k -> when (("saga:" <> name <> ":") `isPrefixOf` k) (kvDel k)) ks

-- | Decide with a pure heuristic when possible; suspend for judgment
-- only on Nothing. The token-efficient shape: the caller is consulted
-- exactly as often as the rules are insufficient.
escalate :: (a -> Maybe Bool) -> (a -> Text) -> a -> M Bool
escalate auto render x = case auto x of
  Just b -> pure b
  Nothing -> approve ("[escalate] " <> render x)

-- | Partition with escalation: rules first, suspensions for the rest.
triageAuto :: (a -> Maybe Bool) -> (a -> Text) -> [a] -> M ([a], [a])
triageAuto auto render xs = do
  tagged <- mapM (\x -> do { b <- escalate auto render x; pure (x, b) }) xs
  pure (map fst (filter snd tagged), map fst (filter (\p -> not (snd p)) tagged))

-- | Pure preflight for triageAuto: which items WOULD escalate under
-- this rule? Tune the rule against this (free) before paying for
-- suspensions one at a time.
escalations :: (a -> Maybe Bool) -> [a] -> [a]
escalations auto = filter (\x -> case auto x of { Nothing -> True; Just _ -> False })

-- | Structural rewrite, gated — the DEFAULT way to rewrite code:
-- plan (ast-grep dry-run), suspend with the full diff, apply only on
-- approval. Pattern-based, so the apply re-matches structurally rather
-- than by byte offset.
rewrite :: Lang -> Text -> Text -> [Text] -> M Value
rewrite lang pat fix paths = do
  plan <- planRw lang pat fix paths
  if isNull plan
    then pure (String "no matches")
    else do
      let clip t = stake 120 (strip t)
      let render (Match t f l _ r) =
            f <> ":" <> pack (show l) <> "\n    - " <> clip t <> "\n    + " <> clip r
      ok <- approve
        ("apply " <> pack (show (len plan)) <> " structural rewrite(s)?\n"
          <> intercalate "\n" (map render plan))
      if ok
        then do
          n <- applyRw lang pat fix paths
          pure (object ["applied" .= n])
        else pure (String "declined")
