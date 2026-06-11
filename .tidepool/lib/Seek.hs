{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
-- | Goal-directed code search as a vocabulary over Schemes.loopM.
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
--   * continuation lifetime: pressure eviction oldest-first, NO time
--     expiry (lib.rs:1412) — 4 probes.
--   * resume is one-shot: conts.remove consumes the entry
--     (lib.rs:1921); the response String crosses the boundary via the
--     session channel (lib.rs:1935) and lands as Value::String — so a
--     continuation dies by eviction OR by its own resume; no replay.
--
-- v2 (affordance fixes from dogfooding v1):
--   * verbs carry usage syntax, rendered verbatim in the prompt — the
--     caller never guesses argument format.
--   * view takes an optional radius, default widened to +/-20 (v1's
--     +/-10 cut a handler mid-body and cost a probe).
--   * older evidence compacts to headers (latest 2 entries full) —
--     the prompt stops growing linearly with probe count.
module Seek where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import Schemes (loopM)
import Asks (oracle)
import Explore (hitsByFile, aroundLine)

-- | A reply language: named verbs over the state. (name, usage,
-- action) — usage is shown verbatim in the prompt; the action
-- receives the reply with the verb name stripped.
type Verb s = (Text, Text, Text -> s -> M s)
type Vocab s = [Verb s]

-- | Interpret one reply against a vocab. @DONE <r>@ terminates with
-- the rest of the reply; otherwise the matching verb runs; an
-- unprefixed reply falls through to the FIRST verb (so the most
-- common probe needs no prefix).
dispatch :: Vocab s -> Text -> s -> M (Either Text s)
dispatch vocab a s
  | "DONE" `isPrefixOf` a = pure (Left (strip (sdrop 4 a)))
  | otherwise = case filter (\(name, _, _) -> name `isPrefixOf` a) vocab of
      ((name, _, run) : _) -> Right <$> run (strip (sdrop (len name) a)) s
      [] -> case vocab of
        ((_, _, run) : _) -> Right <$> run a s
        []                -> pure (Right s)

-- | Split a verb argument on " :: " into parts (each stripped).
argN :: Text -> [Text]
argN = map strip . splitOn " :: "

-- | Split a verb argument on " :: " into (head, tail-or-empty).
arg2 :: Text -> (Text, Text)
arg2 a = case argN a of
  (x : y : _) -> (x, y)
  [x]         -> (x, "")
  _           -> ("", "")

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

-- | grep — regex over files (glob defaults **/*.rs).
grepVerb :: Verb [Text]
grepVerb = ("grep", "grep <regex> :: <glob (default **/*.rs)>", \arg ev -> do
  let (rx, g) = arg2 arg
  hits <- grepGlob rx (if isNull g then "**/*.rs" else g)
  pure (("PROBE " <> arg <> "\n" <> digestHits hits) : ev))

-- | view — window around a line, radius optional (default +/-20).
viewVerb :: Verb [Text]
viewVerb = ("view", "view <file> :: <line> [:: <radius (default 20)>]", \arg ev -> do
  let parts = argN arg
      f = case parts of { (x : _) -> x; _ -> arg }
      ln = case parts of
             (_ : l : _) -> case parseIntM l of { Just n -> n; Nothing -> 1 }
             _ -> 1
      r = case parts of
            (_ : _ : x : _) -> case parseIntM x of { Just n -> n; Nothing -> 20 }
            _ -> 20
  content <- readFile f
  pure (("VIEW " <> f <> ":" <> pack (show ln)
         <> "\n  " <> intercalate "\n  " (aroundLine ln r content)) : ev))

-- | Render the trail chronologically: the most recent 2 entries in
-- full, older entries compacted to their header line (plus the hit
-- summary for probes). The full text already steered past probes;
-- only the summary needs to persist.
renderTrail :: [Text] -> Text
renderTrail ev = intercalate "\n---\n" (reverse (imap render1 ev))
  where
    render1 i e = if i < 2 then e else compactE e
    compactE e = case lines e of
      (h : s : _) | "PROBE" `isPrefixOf` h -> h <> "  [" <> strip s <> "]"
      (h : _) -> h <> "  [compacted]"
      [] -> e

-- | Goal-directed search with a custom vocabulary: loopM over
-- (fuel, trail), one suspension per round.
seekWith :: Vocab [Text] -> Text -> Int -> M Text
seekWith vocab goal fuel = loopM round (fuel, [])
  where
    verbs = intercalate " | " (map (\(_, usage, _) -> "'" <> usage <> "'") vocab)
    round (0, ev) = pure (Left ("FUEL EXHAUSTED. Trail:\n" <> renderTrail ev))
    round (k, ev) = do
      a <- oracle ("[seek] GOAL (fixed): " <> goal
            <> "\n\nEvidence so far:\n"
            <> (if null ev then "(none yet)" else renderTrail ev)
            <> "\n\nReplies: " <> verbs
            <> " | 'DONE <answer>'. Unprefixed reply = first verb. Probes left: " <> pack (show k))
      e <- dispatch vocab (strip a) ev
      pure (case e of
        Left ans  -> Left ("ANSWER: " <> ans <> "\n\n("
                           <> pack (show (fuel - k + 1)) <> " suspensions used)")
        Right ev' -> Right (k - 1, ev'))

-- | The standard instance: grep (default) + view.
seek :: Text -> Int -> M Text
seek = seekWith [grepVerb, viewVerb]
