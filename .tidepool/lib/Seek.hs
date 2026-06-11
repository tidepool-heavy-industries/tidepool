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

-- | Goal-directed search with a custom vocabulary: loopM over
-- (fuel, trail), one suspension per round. Fuel buys probes; when it
-- runs out there is one final conclude-only round, so the last
-- probe's evidence is never wasted.
seekWith :: Vocab [Text] -> Text -> Int -> M Text
seekWith vocab goal fuel = loopM round (fuel, [])
  where
    verbs = intercalate " | " (map (\(_, usage, _) -> "'" <> usage <> "'") vocab)
    header ev = "[seek] GOAL (fixed): " <> goal
          <> "\n\nEvidence so far:\n"
          <> (if null ev then "(none yet)" else renderTrail ev)
    round (0, ev) = do
      a <- oracle (header ev
            <> "\n\nFINAL ROUND — probes exhausted. Reply 'DONE <answer>'; anything else returns the trail unanswered.")
      pure (Left (if "DONE" `isPrefixOf` strip a
        then "ANSWER: " <> strip (sdrop 4 (strip a))
             <> "\n\n(" <> pack (show fuel) <> " probes + conclude)"
        else "NO ANSWER. Trail:\n" <> renderTrail ev))
    round (k, ev) = do
      a <- oracle (header ev
            <> "\n\nReplies: " <> verbs
            <> " | 'DONE <answer>'. Unprefixed reply = first verb. Probes left: " <> pack (show k))
      e <- dispatch vocab (strip a) ev
      pure (case e of
        Left ans  -> Left ("ANSWER: " <> ans <> "\n\n("
                           <> pack (show (fuel - k + 1)) <> " suspensions used)")
        Right ev' -> Right (k - 1, ev'))

-- | The standard instance: grep (default) + view + def + ls.
seek :: Text -> Int -> M Text
seek = seekWith [grepVerb, viewVerb, defVerb, lsVerb]

-- | Typed conclude-round verdict (v5): Q lives in Tidepool.Effects
-- now, so lib modules can define schema'd verbs. The conclude round
-- can ask for a structured verdict instead of prose.
concludeQ :: Q (Text, Double)
concludeQ = (,) <$> txt "answer" <*> num "confidence"

-- | Ask the caller for a typed verdict (server-validated reply).
conclude :: Text -> M (Text, Double)
conclude = askQ concludeQ
