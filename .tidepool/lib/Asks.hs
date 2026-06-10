{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
-- | Continuation verbs: the suspended computation treats the calling LLM
-- as its operating system. State lives in the JIT heap across
-- suspensions; only questions and answers cross the boundary.
module Asks where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects

-- | Ask, expecting plain text back.
oracle :: Text -> M Text
oracle q = do
  v <- ask q
  pure (case v of
    String t -> t
    Bool b -> if b then "true" else "false"
    Null -> ""
    _ -> case v ^? _Number of
      Just n -> pack (show (round n :: Int))
      Nothing -> "")

-- | Yes/no gate. The program pauses until the caller approves.
approve :: Text -> M Bool
approve q = do
  t <- oracle (q <> " [reply yes/no]")
  let s = toLower (strip t)
  pure (s == "yes" || s == "y" || s == "true" || s == "ok")

-- | Menu: present numbered options, get an index back (clamped).
choose :: Text -> [Text] -> M Int
choose q opts = do
  let menu = unlines (map (\(i, o) -> pack (show (i :: Int)) <> ") " <> o) (zipWithIndex opts))
  t <- oracle (q <> "\n" <> menu <> "Reply with the number.")
  let n = case parseIntM (strip t) of { Just k -> k; Nothing -> 0 }
  pure (max' 0 (min' (len opts - 1) n))

-- | Subcontract a unit of work to the caller: it can use ANY of its
-- tools in the gap, then resume with the result (JSON or text).
delegate :: Text -> M Value
delegate task = ask ("[delegate] " <> task)

-- | Sequential labeled delegation: the program fans work out to the
-- caller one item at a time; results aggregate IN THE HEAP, so the
-- caller's context only ever holds the current item.
farmOut :: [(Text, Text)] -> M [(Text, Value)]
farmOut = mapM one
  where
    one (label, task) = do
      v <- delegate (label <> " | " <> task)
      pure (label, v)

-- | Progress checkpoint between batches of work: reply "stop" to cut
-- the run short (returning results so far), anything else to continue.
paced :: Int -> Text -> [M Value] -> M [Value]
paced k label acts = go (1 :: Int) acts
  where
    total = len acts
    go _ [] = pure []
    go i rest = do
      let (batch, more) = splitAt k rest
      rs <- sequence batch
      if isNull more
        then pure rs
        else do
          t <- oracle ("[paced " <> label <> "] " <> pack (show (i * k)) <> "/" <> pack (show total) <> " done. Continue? [anything/stop]")
          if toLower (strip t) == "stop"
            then pure rs
            else do
              rs' <- go (i + 1) more
              pure (rs <> rs')

-- | Interactive bisection: find the FIRST item where the caller answers
-- yes. The program narrows; the caller judges (with full tool access
-- in each gap). O(log n) suspensions.
bisect :: Text -> [Text] -> M (Maybe Text)
bisect q items =
  if isNull items then pure Nothing else go 0 (len items - 1)
  where
    at i = case take 1 (drop i items) of { (x : _) -> Just x; _ -> Nothing }
    go lo hi =
      if lo >= hi
        then pure (at lo)
        else do
          let mid = div (lo + hi) 2
          ok <- approve (q <> " | candidate: " <> (case at mid of { Just t -> t; Nothing -> "" }))
          if ok then go lo mid else go (mid + 1) hi
