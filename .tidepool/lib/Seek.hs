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
--   * resume is one-shot on SUCCESS: a continuation dies by eviction,
--     abort, or a VALID resume. Since typed suspensions (2026-06-11):
--     schema'd asks (askQ/askWith) are validated server-side BEFORE
--     consumption — an invalid reply returns violations and the same
--     continuation_id retries; the canonical validated JSON (not the
--     raw text) is what re-enters the JIT. Raw text is accepted for
--     string/enum schemas; abort is a dedicated tool.
--   * the 30s timeout never kills the eval thread: Err(_elapsed)
--     orphans it to a reaper (2s grace + join, lib.rs:1719) with
--     orphaned_threads accounting; eval() refuses admission at
--     MAX_ORPHANED_EVALS (lib.rs:1751). Leaked-with-accounting.
--   * Fs sandbox = canonicalize-then-prefix-check per handler
--     (tidepool/src/main.rs:253 resolve, :276 root check; siblings
--     at :512 and :923) — found via seek v3, def+ls unused but grep
--     usage syntax + compaction carried a 5-probe run cleanly.
--   * ask end-to-end (seek v4 stress, 6 hops in 5 probes + conclude):
--     send (Ask p) -> Union(tag, req), ask_tag = decls.len()
--     (lib.rs:2062); machine destructures Union + continuation
--     (machine.rs:99-128), HList peels tags (dispatch.rs:281) to
--     AskDispatcher (lib.rs:1441); extract_ask_prompt crosses the
--     heap->Rust boundary; Suspended sent at lib.rs:1447; eval
--     thread blocks on response_rx.recv() (lib.rs:1450).
--
-- v2 (affordance fixes from dogfooding v1):
--   * verbs carry usage syntax, rendered verbatim in the prompt — the
--     caller never guesses argument format.
--   * view takes an optional radius, default widened to +/-20 (v1's
--     +/-10 cut a handler mid-body and cost a probe).
--   * older evidence compacts to headers (latest 2 entries full) —
--     the prompt stops growing linearly with probe count.
--
-- v3: structural verbs — def (rsFn + hsDef definition lookup) and
-- ls (glob), so the move-selector can navigate structure, not just
-- content.
--
-- v4: fuel buys PROBES; exhaustion grants one conclude-only round.
-- (v3 run burned its last probe on the decisive grep, then the loop
-- dumped the trail with no chance to answer — the evidence arrived
-- and was wasted.)
module Seek where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import Control.Monad.Freer (send)
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

-- | Strip one layer of surrounding quotes (LLM drivers emit
-- shell-style 'quoted' regexes; grepGlob would match the quotes
-- literally — observed as a silent all-zero-hits run).
--
-- Implemented via splitOn, NOT stake/sdrop slicing: a sliced Text
-- crossing the effect boundary trips a JIT bridge trap ("expected
-- ByteArray# in ByteArray, got Con(...)") — #313 family, minimal
-- repro: grepGlob (sdrop 1 (stake (len s - 1) s)) g. splitOn output
-- crosses fine (arg2 proves it).
unquote :: Text -> Text
unquote t = un "'" (un "\"" (strip t))
  where un q u = case splitOn q u of
          ["", mid, ""] -> mid
          _             -> u

-- | Split a verb argument on " :: " into parts (stripped, unquoted).
argN :: Text -> [Text]
argN = map (unquote . strip) . splitOn " :: "

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

-- | def — structural definition lookup by name: rsFn (Rust) + hsDef
-- (Haskell), both rooted at the optional path. Shows file:line and
-- the definition head for up to 6 matches.
defVerb :: Verb [Text]
defVerb = ("def", "def <fn-name> [:: <root dir (default .)>]", \arg ev -> do
  let (name, p) = arg2 arg
      roots = if isNull p then ["."] else [p]
  rs <- rsFn name roots
  hs <- hsDef name roots
  let ms = rs ++ hs
      headLine t = case lines t of { (h : _) -> h; _ -> "" }
      render (Match t f l _ _) = f <> ":" <> pack (show l) <> "  " <> stake 90 (strip (headLine t))
  pure (("DEF " <> name <> " (" <> pack (show (length ms)) <> " defs)"
         <> (if null ms then "" else "\n  " <> intercalate "\n  " (map render (take 6 ms)))) : ev))

-- | ls — files matching a glob (first 25).
lsVerb :: Verb [Text]
lsVerb = ("ls", "ls <glob>", \arg ev -> do
  fs <- glob (strip arg)
  pure (("LS " <> arg <> " (" <> pack (show (length fs)) <> " files)"
         <> (if null fs then "" else "\n  " <> intercalate "\n  " (take 25 fs))
         <> (if length fs > 25 then "\n  ..." else "")) : ev))

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

-- | Goal-directed search, generic over the DRIVER (the thing that
-- reads the evidence and picks the next move) and the TRAIL RENDERER.
-- Trail policy depends on the driver's memory: the calling LLM keeps
-- full probe results in its own context, so 'renderTrail' compaction
-- is right for it; a stateless tier-1 driver's ONLY memory is the
-- trail, so it needs 'fullTrail' (field finding 2026-06-11: compaction
-- amputated mini's memory of grep hits and it correctly re-probed).
-- loopM over (fuel, trail). Fuel buys probes; when it runs out there
-- is one final conclude-only round, so the last probe's evidence is
-- never wasted.
seekDriveR :: ([Text] -> Text) -> (Text -> M Text) -> Vocab [Text] -> Text -> Int -> M Text
seekDriveR render driver vocab goal fuel = loopM round (fuel, [], [])
  where
    verbs = intercalate " | " (map (\(_, usage, _) -> "'" <> usage <> "'") vocab)
    header ev = "[seek] GOAL (fixed): " <> goal
          <> "\n\nEvidence so far:\n"
          <> (if null ev then "(none yet)" else render ev)
    round (0, ev, _) = do
      a <- driver (header ev
            <> "\n\nFINAL ROUND — probes exhausted. Reply 'DONE <answer>'; anything else returns the trail unanswered.")
      pure (Left (if "DONE" `isPrefixOf` strip a
        then "ANSWER: " <> strip (sdrop 4 (strip a))
             <> "\n\n(" <> pack (show fuel) <> " probes + conclude)"
        else "NO ANSWER. Trail:\n" <> render ev))
    round (k, ev, mvs) = do
      a <- driver (header ev
            <> "\n\nReplies: " <> verbs
            <> " | 'DONE <answer>'. Unprefixed reply = first verb. Probes left: " <> pack (show k))
      -- Driver-agnostic guards (field findings, 2026-06-11):
      -- 1. Conclusions require evidence — a DONE against an empty trail
      --    is a prior, not an answer (tier-1 pattern-matched an instant
      --    DONE from the question alone).
      -- 2. No duplicate moves — a stateless driver can loop (4x
      --    identical 'ls' observed); reject mechanically, surface the
      --    rejection in the trail so the driver sees it.
      let a' = strip a
      if "DONE" `isPrefixOf` a' && null ev
        then pure (Right (k - 1,
          ["REJECTED: DONE with an empty evidence trail — that is a guess, not a finding. Probe first; the answer must cite gathered evidence."], mvs))
        else if a' `elem` mvs
          then pure (Right (k - 1,
            ("REJECTED duplicate move: " <> a' <> " — already probed; pick a DIFFERENT probe or DONE.") : ev, mvs))
          else do
            e <- dispatch vocab a' ev
            pure (case e of
              Left ans  -> Left ("ANSWER: " <> ans <> "\n\n("
                                 <> pack (show (fuel - k + 1)) <> " rounds)")
              Right ev' -> Right (k - 1, ev', a' : mvs))

-- | Full chronological trail, nothing compacted — for stateless
-- drivers whose only memory is the trail itself.
fullTrail :: [Text] -> Text
fullTrail ev = intercalate "\n---\n" (reverse ev)

-- | Custom-driver search with the compacting renderer (right for
-- caller-LLM drivers, which keep full results in their own context).
seekDrive :: (Text -> M Text) -> Vocab [Text] -> Text -> Int -> M Text
seekDrive = seekDriveR renderTrail

-- | Caller-driven search with a custom vocabulary (one suspension
-- per round).
seekWith :: Vocab [Text] -> Text -> Int -> M Text
seekWith = seekDrive oracle

-- | The standard instance: grep (default) + view + def + ls.
seek :: Text -> Int -> M Text
seek = seekWith [grepVerb, viewVerb, defVerb, lsVerb]

-- | The standard vocabulary, exported for custom drivers.
stdVocab :: Vocab [Text]
stdVocab = [grepVerb, viewVerb, defVerb, lsVerb]

-- | Typed conclude-round verdict (v5): Q lives in Tidepool.Effects
-- now, so lib modules can define schema'd verbs. The conclude round
-- can ask for a structured verdict instead of prose.
concludeQ :: Q (Text, Double)
concludeQ = (,) <$> txt "answer" <*> num "confidence"

-- | Ask the caller for a typed verdict (server-validated reply).
conclude :: Text -> M (Text, Double)
conclude = askQ concludeQ
