{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
-- | Goal-directed code search as a vocabulary over Library.loopM.
--
-- The driver is generic (loopM: iterate until the body returns a
-- result); this module supplies the search-specific parts: a 'Vocab'
-- of read-only probe verbs over an evidence trail, a digesting
-- renderer (counts + exemplars, never corpora), and fuel. The caller
-- is the coalgebra: each round suspends via 'oracle', the reply picks
-- the next probe or finishes with @DONE <answer>@.
--
-- Field notes (2026-06-11, v1 runs):
--   * ParkedStream per-id removal on exhaustion (host_fns.rs:2601),
--     correcting a stale "lives to teardown" belief — 4 probes.
--   * continuation lifetime: resume-remove (lib.rs:1771) + pressure
--     eviction oldest-first, NO time expiry (lib.rs:1412) — 4 probes.
--
-- With an inline-LLM provider configured, tier 1 slots between the
-- pure default and the suspension (?? cascade): the model proposes
-- the next reply, the caller is asked only on low confidence.
module Seek where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import Schemes (loopM)
import Asks (oracle)
import Explore (hitsByFile, aroundLine)

-- | A reply language: named verbs over the state. A verb's action
-- receives the reply with the verb name stripped.
type Vocab s = [(Text, Text -> s -> M s)]

-- | Interpret one reply against a vocab. @DONE <r>@ terminates with
-- the rest of the reply; otherwise the matching verb runs; an
-- unprefixed reply falls through to the FIRST verb (so the most
-- common probe needs no prefix).
dispatch :: Vocab s -> Text -> s -> M (Either Text s)
dispatch vocab a s
  | "DONE" `isPrefixOf` a = pure (Left (strip (sdrop 4 a)))
  | otherwise = case filter (\(name, _) -> name `isPrefixOf` a) vocab of
      ((name, run) : _) -> Right <$> run (strip (sdrop (len name) a)) s
      [] -> case vocab of
        ((_, run) : _) -> Right <$> run a s
        []             -> pure (Right s)

-- | Split a verb argument on " :: " into (head, tail-or-empty).
arg2 :: Text -> (Text, Text)
arg2 a = case splitOn " :: " a of
  [x, y] -> (strip x, strip y)
  _      -> (strip a, "")

-- | Digest grep hits: counts + per-file density + exemplar lines.
-- Shows ALL hits when few (<=8); never floods the prompt when many.
digestHits :: [(Text, Int, Text)] -> Text
digestHits hs =
  let byFile = hitsByFile hs
      small = length hs <= 8
      shown = if small then hs else take 5 hs
      label = if small then "all hits:" else "exemplars:"
      lns = map (\(f, l, t) -> f <> ":" <> pack (show l) <> "  " <> stake 90 (strip t)) shown
  in "hits=" <> pack (show (length hs)) <> " across " <> pack (show (length byFile)) <> " files"
     <> "\n top files: " <> pack (show (take 5 byFile))
     <> (if null lns then "" else "\n " <> label <> "\n  " <> intercalate "\n  " lns)

-- | grep <regex> :: <glob> — regex over files (glob defaults **/*.rs).
grepVerb :: (Text, Text -> [Text] -> M [Text])
grepVerb = ("grep", \arg ev -> do
  let (rx, g) = arg2 arg
  hits <- grepGlob rx (if isNull g then "**/*.rs" else g)
  pure (("PROBE " <> arg <> "\n" <> digestHits hits) : ev))

-- | view <file> :: <line> — ~20-line window around a line.
viewVerb :: (Text, Text -> [Text] -> M [Text])
viewVerb = ("view", \arg ev -> do
  let (f, lnT) = arg2 arg
      ln = case parseIntM lnT of { Just n -> n; Nothing -> 1 }
  content <- readFile f
  pure (("VIEW " <> f <> ":" <> lnT <> "\n  "
         <> intercalate "\n  " (aroundLine ln 10 content)) : ev))

-- | Goal-directed search with a custom vocabulary: loopM over
-- (fuel, trail), one suspension per round.
seekWith :: Vocab [Text] -> Text -> Int -> M Text
seekWith vocab goal fuel = loopM round (fuel, [])
  where
    verbs = intercalate " | " (map (\(n, _) -> "'" <> n <> " ...'") vocab)
    trail ev = intercalate "\n---\n" (reverse ev)
    round (0, ev) = pure (Left ("FUEL EXHAUSTED. Trail:\n" <> trail ev))
    round (k, ev) = do
      a <- oracle ("[seek] GOAL (fixed): " <> goal
            <> "\n\nEvidence so far:\n"
            <> (if null ev then "(none yet)" else trail ev)
            <> "\n\nReplies: " <> verbs <> " | 'DONE <answer>'. Probes left: " <> pack (show k))
      e <- dispatch vocab (strip a) ev
      pure (case e of
        Left ans  -> Left ("ANSWER: " <> ans <> "\n\n("
                           <> pack (show (fuel - k + 1)) <> " suspensions used)")
        Right ev' -> Right (k - 1, ev'))

-- | The standard instance: grep (default) + view.
seek :: Text -> Int -> M Text
seek = seekWith [grepVerb, viewVerb]
