{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
-- | LSP-driven exploration: a bounded, type-resolved code graph you walk in
-- Haskell and steer with a heuristic → local-LLM → human cascade.
--
-- The currency is `LspNode` (from Tidepool.Effects); every nav op is
-- `LspNode -> M [LspNode]`, so frontiers fold with `concatMapM`/`loopM` and the
-- `steer` cascade prunes each one. Because a whole walk is one `M` value, a
-- single human escalation parks the entire exploration in the JIT heap and
-- resumes — the discover/explore loop is itself a resumable coroutine.
module Lsp where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import Schemes (loopM)
import Diff (applyDiff)
import qualified Data.Map.Strict as Map
import qualified Tidepool.Data.Text as T

-- ===== small generic helpers =====
-- (`concatMapM` comes from Tidepool.Prelude)

-- | Safe index into a list; Nothing when the index is out of bounds.
atMay :: [a] -> Int -> Maybe a
atMay xs i
  | i < 0     = Nothing
  | otherwise = case drop i xs of { (x : _) -> Just x; _ -> Nothing }

-- | First path segment — a crate/dir name, for the cheap same-crate rule.
crateOf :: LspNode -> Text
crateOf n = case splitOn "/" (nodeFile n) of { (c : _) -> c; _ -> "" }

-- | Crude test-path heuristic (a cheap rule-tier predicate).
isTest :: LspNode -> Bool
isTest n = isInfixOf "tests/" f || isInfixOf "/test" f || isInfixOf "_test" f
  where f = nodeFile n

-- | Unwrapping helpers: the effect ops return `Maybe [LspNode]` (Nothing = the
-- node isn't analyzable). For plain graph-walking you usually want "[] = stop",
-- so these collapse Nothing → []. The honest `lsp*` primitives stay available
-- when you want to distinguish "not a function" from "no callers".
callersOf, calleesOf, refsOf :: LspNode -> M [LspNode]
callersOf = fmap (fromMaybe []) . lspCallers
calleesOf = fmap (fromMaybe []) . lspCallees
refsOf    = fmap (fromMaybe []) . lspRefs

-- ===== the steer cascade =====

-- | Escalate only as far as needed: pure rule, else a local model, else
-- suspend to the caller. Most calls never leave the first tier.
steer :: (a -> Maybe b)        -- ^ rule:  pure heuristic (Nothing = abstain)
      -> (a -> M (Maybe b))    -- ^ model: local llm, gated on confidence
      -> (a -> M b)            -- ^ human: ask — parks the eval
      -> a -> M b
steer rule model human x = case rule x of
  Just b  -> pure b
  Nothing -> do
    m <- model x
    case m of
      Just b  -> pure b
      Nothing -> human x

-- | Local-model yes/no with a confidence gate. `Nothing` = model unavailable
-- or below threshold → the cascade escalates.
judgeBool :: Double -> Text -> M (Maybe Bool)
judgeBool thresh prompt = do
  r <- tryLlm (SObj [("verdict", SBool), ("confidence", SNum)]) prompt
  pure (case r of
    Right v -> case (v ^? key "confidence" . _Double, v ^? key "verdict" . _Bool) of
      (Just c, Just b) -> if c >= thresh then Just b else Nothing
      _                -> Nothing
    Left _ -> Nothing)

-- | Suspend to the caller for a yes/no.
askBool :: Text -> M Bool
askBool prompt = do
  v <- ask (SObj [("yes", SBool)]) prompt
  pure (maybe False id (v ^? key "yes" . _Bool))

-- ===== graph walk core =====

-- | BFS `edge` outward from `root` up to `depth`, keeping frontier nodes that
-- pass `keep`. The shared engine behind `explore`/`chart`.
walk :: (LspNode -> M [LspNode]) -> (LspNode -> M Bool) -> Int -> LspNode -> M [LspNode]
walk edge keep depth root = loopM step (depth, [root], [])
  where
    step (d, frontier, acc) =
      if d <= (0 :: Int)
        then pure (Left acc)
        else do
          nxt  <- concatMapM edge frontier
          kept <- filterM keep nxt
          pure (Right (d - 1, kept, acc ++ kept))

-- ===== ready-made explorers =====

-- | Walk the caller graph, pruning each frontier with the cascade: skip tests
-- by rule, else the local model judges "on the path to GOAL?", else ask.
explore :: Int -> Text -> LspNode -> M [LspNode]
explore depth goal = walk callersOf onPath depth
  where
    onPath = steer
      (\n -> if isTest n then Just False else Nothing)
      (\n -> judgeBool 0.7 ("Is " <> nodeName n <> " on the path to " <> goal <> "?  " <> nodeText n))
      (\n -> askBool ("Follow " <> nodeName n <> "?  " <> nodeText n))

-- | Resolve a name to its one intended definition: unique-after-rule, else the
-- local model ranks, else the human chooses. Returns a `LspNode` to navigate from.
the :: Text -> Text -> M (Maybe LspNode)
the name intent = do
  defs <- lspWhere name
  case filter (not . isTest) defs of
    []  -> pure Nothing
    [n] -> pure (Just n)
    ns  -> Just <$> steer (\_ -> Nothing) (\_ -> pickModel ns) (\_ -> pickHuman ns) ()
  where
    menu ns = intercalate "\n" (imap (\i n -> showT i <> ") " <> nodeContainer n <> "  " <> nodeText n) ns)
    pickModel ns = do
      r <- tryLlm (SObj [("index", SNum), ("confidence", SNum)])
             ("Pick the definition matching: " <> intent <> "\n" <> menu ns)
      pure (case r of
        Right v -> case (v ^? key "confidence" . _Double, v ^? key "index" . _Double) of
          (Just c, Just i) -> if c >= 0.7 then atMay ns (round i) else Nothing
          _                -> Nothing
        Left _ -> Nothing)
    pickHuman ns = do
      v <- ask (SObj [("index", SNum)]) ("Which " <> name <> "?\n" <> menu ns)
      pure (maybe (head ns) id (v ^? key "index" . _Double >>= (atMay ns . round)))

-- | Impact-assessed rename: classify each reference (in-crate=safe rule → local
-- model judges cross-crate → human confirms), mutate only on approval, apply
-- via the existing diff path. Returns a structured outcome.
saferRename :: LspNode -> Text -> M Value
saferRename sym new = do
  refs  <- refsOf sym
  risky <- filterM (steer
             (\r -> if crateOf sym == crateOf r then Just False else Nothing)
             (\r -> judgeBool 0.8 ("Risky to rename this use?  " <> nodeText r))
             (\r -> askBool ("Risky? confirm:  " <> nodeText r))) refs
  proceed <- if null risky
               then pure True
               else askBool (pack (show (length risky)) <> " risky site(s) — proceed?")
  if proceed
    then do
      md <- lspRename sym new
      case md of
        Just d  -> do
          report <- applyDiff d
          pure (object ["applied" .= report, "risky" .= length risky])
        Nothing -> pure (object ["rename_unsupported" .= True])
    else pure (object ["aborted" .= True, "risky" .= toJSON risky])

-- | Subsystem cartographer: BFS the callee graph, hover each node, have the
-- local model write a one-line role (escalating unclear ones), emit a map.
chart :: Int -> LspNode -> M Value
chart depth entry = do
  ns   <- walk calleesOf (\_ -> pure True) depth entry
  rows <- mapM describe ns
  pure (toJSON rows)
  where
    describe n = do
      sig  <- lspHover n
      role <- steer (\_ -> Nothing) (\_ -> roleModel n sig) (\_ -> roleHuman n) ()
      pure (object ["sym" .= nodeName n, "file" .= nodeFile n, "role" .= role])
    roleModel n sig = do
      r <- tryLlm (SObj [("role", SStr), ("confidence", SNum)])
             ("One short line: what does " <> nodeName n <> " do?\n" <> maybe (nodeText n) id sig)
      pure (case r of
        Right v -> case (v ^? key "confidence" . _Double, v ^? key "role" . _String) of
          (Just c, Just s) -> if c >= 0.6 then Just s else Nothing
          _                -> Nothing
        Left _ -> Nothing)
    roleHuman n = ask (SObj [("role", SStr)]) ("Role of " <> nodeName n <> "?  " <> nodeText n)
                    <&> \v -> maybe "" id (v ^? key "role" . _String)

-- ===== workspace scoping (2026-07-01, from the chart-noise finding) =====

-- | In-workspace test: the daemon reports workspace files path-RELATIVE and
-- external ones (rustup stdlib, registry deps) absolute.
isLocal :: LspNode -> Bool
isLocal n = not (T.isPrefixOf "/" (nodeFile n))

-- | Edge variants that stay inside the workspace — the right default for
-- charts and walks (stdlib Option/Vec noise otherwise dominates every frontier).
localCallees, localCallers, localRefs :: LspNode -> M [LspNode]
localCallees = fmap (filter isLocal) . calleesOf
localCallers = fmap (filter isLocal) . callersOf
localRefs    = fmap (filter isLocal) . refsOf

-- | `the`, loud: resolve a symbol name to its ONE definition or error with
-- the reason. For flows where absence means a typo, not a branch.
findDef :: Text -> Text -> M LspNode
findDef name intent =
  the name intent >>= maybe (error ("no workspace definition found for '" <> name <> "'")) pure

-- | Fully-autonomous subsystem chart: workspace-only callee walk, one-line
-- roles from the local model with a hover-first-line fallback — NEVER
-- suspends (bulk maps shouldn't park on a human). Junk-guard: filler roles
-- ("function description", "summary") fall back to the hover line too.
chartAuto :: Int -> LspNode -> M Value
chartAuto depth entry = do
  ns   <- walk localCallees (\_ -> pure True) depth entry
  rows <- mapM describe (dedupeOn nodeKey ns)
  pure (toJSON rows)
  where
    nodeKey n = nodeFile n <> ":" <> nodeName n
    describe n = do
      sig <- lspHover n
      let fallback = firstUseful (maybe (nodeText n) id sig)
      r <- tryLlm (SObj [("role", SStr), ("confidence", SNum)])
             ("One short specific line: what does " <> nodeName n <> " do? If unsure, confidence 0.\n"
              <> maybe (nodeText n) id sig)
      let role = case r of
            Right v -> case (v ^? key "confidence" . _Double, v ^? key "role" . _String) of
              (Just c, Just s) | c >= 0.6 && not (junkRole s) -> s
              _ -> fallback
            Left _ -> fallback
      pure (object ["sym" .= nodeName n, "file" .= nodeFile n, "line" .= nodeLine n, "role" .= role])
    junkRole s = let l = T.toLower s
                 in T.isInfixOf "description" l || l == "summary" || l == "function" || T.length l < 8
    -- Hover text is fenced markdown ("```rust\n<sig>\n```\n\ndocs…"): the
    -- useful fallback line is the first non-fence, non-blank one.
    firstUseful t = case filter useful (lines t) of { (x : _) -> T.strip x; _ -> t }
      where useful l = not (T.isPrefixOf "```" (T.strip l)) && not (T.null (T.strip l))

-- | Order-preserving dedupe by key (frontiers revisit nodes via diamond edges).
dedupeOn :: Eq b => (a -> b) -> [a] -> [a]
dedupeOn f = go []
  where
    go _ [] = []
    go seen (x : xs)
      | f x `elem` seen = go seen xs
      | otherwise       = x : go (f x : seen) xs

-- | One-verb blast radius: the definition + every use site, grouped per file.
-- @blastRadius "remap_generated_coords" "the coordinate remapper"@
blastRadius :: Text -> Text -> M Value
blastRadius name intent = do
  d  <- findDef name intent
  rs <- refsOf d
  let byFile = sortOn (negate . snd) (Map.toList (Map.fromListWith (+) [ (nodeFile r, 1 :: Int) | r <- rs ]))
  pure (object [ "def" .= (nodeFile d <> ":" <> showT (nodeLine d))
               , "totalRefs" .= length rs
               , "byFile" .= toJSON byFile ])
