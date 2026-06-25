-- Recovered fable-era (2026-06-10..12) continuation combinators.
-- Deleted in commits 19e4ea1 and b6d3fb9; extracted from git history
-- and cross-referenced against fable session evals.
--
-- Four modules + one generated-Haskell fragment:
--   Asks.hs    — primitive continuation verbs (oracle, delegate, farmOut, bisect)
--   Flow.hs    — stateful control flow (watch, saga, escalate, rewrite)
--   Seek.hs    — goal-directed code search (loopM + vocab + fuel)
--   Glue.hs    — semantic-glue verbs (grepSift, diagnose) [tier-1 dependent]
--   ??/survey  — tier-1 cascade (generated into Tidepool.Effects by lib.rs)


-- =====================================================================
-- Asks.hs — continuation verbs
-- =====================================================================
-- The suspended computation treats the calling LLM as its operating
-- system. State lives in the JIT heap across suspensions; only
-- questions and answers cross the boundary.

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


-- =====================================================================
-- Flow.hs — stateful control flow over continuations
-- =====================================================================
-- Sentinels (heap-held snapshots diffed on demand), sagas
-- (KV-checkpointed resumable workflows), and escalation (suspend only
-- for genuine ambiguity).

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


-- =====================================================================
-- Seek.hs — goal-directed code search
-- =====================================================================
-- loopM-driven investigation with a vocab of probe verbs (grep, view,
-- def, ls), fuel budget, evidence compaction, and duplicate-move
-- rejection.
--
-- Field notes (2026-06-11, v1–v5 runs):
--   * ParkedStream per-id removal on exhaustion (host_fns.rs:2601),
--     correcting a stale "lives to teardown" belief — 4 probes.
--   * continuation lifetime: pressure eviction oldest-first, NO time
--     expiry (lib.rs:1412) — 4 probes.
--   * resume is one-shot on SUCCESS: a continuation dies by eviction,
--     abort, or a VALID resume. Typed suspensions (askQ/askWith) are
--     validated server-side BEFORE consumption — an invalid reply
--     returns violations and the same continuation_id retries.
--   * the 30s timeout never kills the eval thread: Err(_elapsed)
--     orphans it to a reaper (2s grace + join) with orphaned_threads
--     accounting.
--   * Fs sandbox = canonicalize-then-prefix-check per handler.
--   * ask end-to-end (seek v4 stress, 6 hops in 5 probes + conclude):
--     send (Ask p) -> Union(tag, req), ask_tag = decls.len();
--     machine destructures Union + continuation; HList peels tags
--     to AskDispatcher; extract_ask_prompt crosses the heap->Rust
--     boundary; Suspended sent; eval thread blocks on response_rx.
--
-- v2: verbs carry usage syntax; view radius widened to +/-20;
--     older evidence compacts to headers.
-- v3: structural verbs (def, ls).
-- v4: fuel buys PROBES; exhaustion grants one conclude-only round.
-- v5: typed conclude verdict via askQ.

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
-- literally).
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
-- (Haskell), both rooted at the optional path.
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
-- full, older entries compacted to their header line.
renderTrail :: [Text] -> Text
renderTrail ev = intercalate "\n---\n" (reverse (imap render1 ev))
  where
    render1 i e = if i < 2 then e else compactE e
    compactE e = case lines e of
      (h : s : _) | "PROBE" `isPrefixOf` h -> h <> "  [" <> strip s <> "]"
      (h : _) -> h <> "  [compacted]"
      [] -> e

-- | Full chronological trail, nothing compacted — for stateless
-- drivers whose only memory is the trail itself.
fullTrail :: [Text] -> Text
fullTrail ev = intercalate "\n---\n" (reverse ev)

-- | Goal-directed search, generic over the DRIVER and TRAIL RENDERER.
-- loopM over (fuel, trail, moves). Fuel buys probes; exhaustion grants
-- one conclude-only round. Duplicate moves are rejected mechanically.
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

-- | Custom-driver search with the compacting renderer.
seekDrive :: (Text -> M Text) -> Vocab [Text] -> Text -> Int -> M Text
seekDrive = seekDriveR renderTrail

-- | Caller-driven search with a custom vocabulary.
seekWith :: Vocab [Text] -> Text -> Int -> M Text
seekWith = seekDrive oracle

-- | The standard instance: grep (default) + view + def + ls.
seek :: Text -> Int -> M Text
seek = seekWith [grepVerb, viewVerb, defVerb, lsVerb]

-- | The standard vocabulary, exported for custom drivers.
stdVocab :: Vocab [Text]
stdVocab = [grepVerb, viewVerb, defVerb, lsVerb]

-- | Typed conclude-round verdict (v5).
concludeQ :: Q (Text, Double)
concludeQ = (,) <$> txt "answer" <*> num "confidence"

-- | Ask the caller for a typed verdict (server-validated reply).
conclude :: Text -> M (Text, Double)
conclude = askQ concludeQ


-- =====================================================================
-- Glue.hs — semantic-glue verbs (tier-1 LLM dependent)
-- =====================================================================
-- These required the ?? operator (tier-1 cascade). Code does what the
-- model shouldn't (iteration, exactness, aggregation); the model does
-- what code can't (semantic judgment).

module Glue where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects

-- | Semantic grep: regex for recall, tier-1 judgment for precision.
grepSift :: Text -> Text -> Text -> M ([(Text, Int, Text)], Int)
grepSift intent rx g = do
  hits <- grepGlob rx g
  if length hits <= 3
    then pure (hits, 0)
    else do
      (keep, dropped) <- sift yn render (take 60 hits)
      pure (keep, length dropped)
  where
    render (f, l, t) =
      "Is this code line plausibly relevant to: \"" <> intent <> "\"?\n"
        <> f <> ":" <> pack (show l) <> "  " <> stake 160 (strip t)

-- | Match an error message against the known-gotcha catalog.
diagnose :: Text -> M Value
diagnose err =
  obj schema ?? (catalog <> "\n\nERROR TO DIAGNOSE:\n" <> stake 2500 err)
  where
    schema = SObj
      [ ("gotcha", SEnum gotchaNames)
      , ("why", SStr)
      , ("suggestion", SStr)
      ]
    gotchaNames =
      [ "read-gmp-ffi"
      , "takeWhile-partial-application"
      , "cycle-unresolved-external"
      , "double-breakOn-case-trap"
      , "non-tail-recursion-overflow"
      , "constructor-tag-mismatch"
      , "jit-thread-crash"
      , "effect-error"
      , "none-of-these"
      ]
    catalog = intercalate "\n"
      [ "Known tidepool JIT gotchas (match the ERROR below to ONE):"
      , "- read-gmp-ffi: COMPILE error 'Unsupported FFI call: ...gmpn...' — `read`/`reads` pull GMP Integer ops. Fix: parseInt/parseDouble."
      , "- takeWhile-partial-application: no error, SILENTLY WRONG results — T.takeWhile/T.dropWhile partially applied. Fix: use the Prelude shadows."
      , "- cycle-unresolved-external: runtime 'unresolved variable VarId(0x...)' — `cycle` is an unresolved external. Fix: manual recursion."
      , "- double-breakOn-case-trap: 'case trap: tag mismatch' — second T.breakOn on the sdrop of the first's remainder. Fix: inline in one do-block."
      , "- non-tail-recursion-overflow: 'stack overflow' — non-tail recursion past ~10-20K frames. Fix: make it tail-recursive."
      , "- constructor-tag-mismatch: SIGILL / 'case trap' — a case hit a value shape no branch matches. Usually a compiler bug."
      , "- jit-thread-crash: 'eval thread crashed' — JIT compiler bug. Check .tidepool/crash.log."
      , "- effect-error: 'effect dispatch error' — an effect handler failed. Not a JIT bug; fix the call."
      , "- none-of-these: anything else."
      ]


-- =====================================================================
-- Tier-1 cascade — generated Haskell (was in Tidepool.Effects)
-- =====================================================================
-- These were emitted by tidepool-mcp/src/lib.rs llm_decl() into the
-- generated Tidepool.Effects module. Depended on the Llm effect being
-- in the stack. Removed when ?? was removed.

-- data Judged a = Sure a | Unsure Double a

-- Internal: augment schema with self-assessment rubric.
-- h_aug :: Schema -> Schema
-- h_aug (SObj fs) = SObj (fs ++ [("_understood", SBool), ("_confident", SBool), ("_unambiguous", SBool)])
-- h_aug s = SObj [("value", s), ("_understood", SBool), ("_confident", SBool), ("_unambiguous", SBool)]

-- Internal: extract confidence from rubric fields.
-- h_conf :: Value -> Double
-- h_conf v =
--   let b k = case v ^? key k . _Bool of { Just True -> 1.0; _ -> 0.0 }
--   in (b "_understood" + b "_confident" + b "_unambiguous") / 3.0

-- Internal: strip rubric fields from result.
-- h_strip :: Value -> Value
-- h_strip (Object kvs) = Object (KM.delete (KM.fromText "_unambiguous") (KM.delete (KM.fromText "_confident") (KM.delete (KM.fromText "_understood") kvs)))
-- h_strip v = v

-- ?? : ask the model, auto-escalate to caller on low confidence.
-- infixl 1 ??
-- (??) :: Q a -> Text -> M a
-- (Q schema parse threshold) ?? prompt = do
--   r <- llmJson prompt (h_aug schema)
--   let c = h_conf r
--   v <- if c >= threshold then pure (h_strip r)
--        else h_wrap schema <$> askWith (object ["schema" .= schemaToValue schema])
--               (prompt <> "\n[draft " <> pack (showDouble c) <> "]: " <> show (h_strip r))
--   pure (parse v)

-- ?! : like ?? but preserves the confidence verdict.
-- infixl 1 ?!
-- (?!) :: Q a -> Text -> M (Judged a)
-- (Q schema parse threshold) ?! prompt = do
--   r <- llmJson prompt (h_aug schema)
--   let c = h_conf r
--   if c >= threshold
--     then pure (Sure (parse (h_strip r)))
--     else do
--       v <- askWith (object ["schema" .= schemaToValue schema])
--              (prompt <> "\n[draft " <> pack (showDouble c) <> "]: " <> show (h_strip r))
--       pure (Unsure c (parse (h_wrap schema v)))

-- Batch helpers built on ??:
-- triage :: Q b -> (a -> Text) -> [a] -> M [(a, b)]
-- triage q render = mapM (\x -> (,) x <$> (q ?? render x))
--
-- survey :: Eq b => Q b -> (a -> Text) -> [a] -> M [(b, Int)]
-- survey q render xs = do
--   bs <- mapM (\x -> q ?? render x) xs
--   pure (tallyList bs)
--
-- sift :: Q Bool -> (a -> Text) -> [a] -> M ([a], [a])
-- sift q render xs = do
--   tagged <- mapM (\x -> (,) x <$> (q ?? render x)) xs
--   pure (map fst (filter snd tagged), map fst (filter (not . snd) tagged))

-- Usage patterns (all used ?? under the hood):
--   pick cats ?? prompt      -- classify (M Text)
--   yn ?? prompt             -- yes/no (M Bool)
--   obj schema ?? prompt     -- structured extraction (M Value)
--   txt "field" ?? prompt    -- single text field (M Text)
--   num "field" ?? prompt    -- single number field (M Double)
--   (,) <$> pick cs <*> num "n" ?? p  -- Applicative merge, one call
--   bar 0.95 q ?? prompt     -- raise threshold
