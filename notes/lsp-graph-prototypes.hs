-- LSP-graph prototypes — session handoff (2026-06-25, v3 LSP effect)
-- =====================================================================
-- These ran as INLINE eval `helpers` against the live v3 LSP effect; they are
-- NOT yet in `.tidepool/lib`. This file is the working source to PROMOTE into
-- two lib modules:
--   * `LspGraph.hs` — deterministic exhaustive structure (closure/callTree/leaves)
--   * `Lsp.hs` additions — `lspSeek` (model-driven investigation), next to the
--     committed `explore`/`chart`/`steer`/`callersOf` family.
--
-- Provenance + design rationale: memory notes
--   lsp-graph-idioms-and-where-llm-helps, structured-node-choice-for-llm-nav,
--   lsp-v3-ergonomics-and-missing-ops, lsp-effect-friction.
--
-- Context every snippet assumes (already in the committed surface):
--   * Node {nodeName,nodeContainer,nodeKind,nodeFile,nodePos,nodeText}; nodeLine helper.
--   * lspWhere :: Text -> M [Node]
--   * lspCallers/lspCallees/lspRefs :: Node -> M (Maybe [Node])   -- v3: Maybe!
--   * lspDef :: Node -> M (Maybe Node); lspHover :: Node -> M (Maybe Text)
--   * lspRename :: Node -> Text -> M (Maybe Text)
--   * callersOf/calleesOf/refsOf :: Node -> M [Node]  -- = fmap (fromMaybe []) . lsp*  (in Lsp.hs)
--   * Schema: SObj/SEnum/SStr/SNum/SBool; ask/llm/tryLlm :: Schema -> Text -> M (...)
--   * loopM (Schemes), imap/nubBy/atMay (Prelude/Lsp), concatMapM (Prelude).
-- Gotchas that bit (see lsp-effect-friction): `show` returns Text → use <>, not ++;
-- T.drop/T.breakOn are qualified; name-clash with auto-imported Library (census/isTest/
-- fanOut); `oracle`/`approve`/`??` are deleted — define oracle over `ask` if needed.


-- ===== shared predicates / edges ====================================

-- workspace + callable. NOTE: callTree/leaves/closure bake a crate/workspace
-- filter inline; the lib version should take `keep :: Node -> Bool` as a param.
wk :: Node -> Bool
wk n = isPrefixOf "tidepool" (nodeFile n) && (nodeKind n == "function" || nodeKind n == "method")

-- example predicates for the audit
isUnsafe :: Node -> Bool
isUnsafe n = isInfixOf "unsafe fn" (nodeText n)


-- ===== DETERMINISTIC: closure / reachableMatching ====================
-- The valuable, COMPLETE play. Runs to FIXPOINT (frontier empties); `hitCap`
-- reports truncation rather than silently lying; `rounds` proves convergence.
-- COMPLETE ONLY ON STATIC-CALL SUBSYSTEMS (tidepool-eval/-optimize/-mcp/-lsp,
-- diff/Myers). Past the JIT/effect-machine boundary the call graph misses the
-- RUST_ROOTS indirect edges and the closure under-reports — verified.

closure :: Int -> (Node -> M [Node]) -> [Node] -> M ([Node], Bool, Int)
closure cap edge roots = go roots (map nodeName roots) roots 0
  where
    go [] _ acc r = pure (acc, False, r)
    go frontier seen acc r =
      if length seen >= cap then pure (acc, True, r)
      else do
        next <- concatMapM edge frontier
        let fresh = nubBy (\a b -> nodeName a == nodeName b)
                          [ n | n <- next, wk n, not (elem (nodeName n) seen) ]
        if null fresh then pure (acc, False, r + 1)
        else go fresh (seen ++ map nodeName fresh) (acc ++ fresh) (r + 1)

-- the AUDIT: "every X reachable from Y". edge = calleesOf (down) / callersOf (up).
-- e.g.  reachableMatching calleesOf isUnsafe root
reachableMatching :: (Node -> M [Node]) -> (Node -> Bool) -> Node -> M Value
reachableMatching edge p root = do
  (reached, capped, rounds) <- closure 500 edge [root]
  let hits = filter p reached
  pure (object [ "reachable" .= length reached, "rounds_to_fixpoint" .= rounds
               , "capped" .= capped, "matching" .= length hits
               , "hits" .= map (\n -> nodeName n <> " @ " <> nodeFile n) hits ])


-- ===== ana → bounded TREE: callTree ==================================
-- Unfold callees into a depth-bounded, crate-pruned, cycle-guarded (ancestor
-- path) tree; fold to nested JSON. Recovers a subsystem's dispatch structure.
-- Verified on `eval` (tidepool-eval): eval→eval_settled→eval_at→{force,
-- dispatch_primop,enqueue_jump,…}, self-recursion caught by the ancestor guard.

callTree :: Int -> Text -> Node -> M Value
callTree depth crateP root = go depth [nodeName root] root
  where
    keep anc c = isPrefixOf crateP (nodeFile c)
                 && (nodeKind c == "function" || nodeKind c == "method")
                 && not (elem (nodeName c) anc)
    go d anc n =
      if d <= (0 :: Int) then pure (object ["fn" .= nodeName n, "more" .= ("…" :: Text)])
      else do
        kids <- calleesOf n <&> filter (keep anc)
        let kids' = nubBy (\a b -> nodeName a == nodeName b) kids
        subs <- mapM (go (d - 1) (nodeName n : anc)) kids'
        pure (if null subs then object ["fn" .= nodeName n]
              else object ["fn" .= nodeName n, "calls" .= subs])


-- ===== cata → LEAVES: leaves =========================================
-- Fold the crate-scoped, cycle-guarded closure to its TERMINAL functions = a
-- subsystem's operational primitives. Verified on `eval` → 25 primitives
-- (bin_op_*/cmp_*/expect_*/force/alloc/read/write/enqueue_jump/…).

leaves :: Int -> Text -> Node -> M [Text]
leaves depth crateP root = go depth [nodeName root] root <&> nub
  where
    keep anc c = isPrefixOf crateP (nodeFile c)
                 && (nodeKind c == "function" || nodeKind c == "method")
                 && not (elem (nodeName c) anc)
    go d anc n =
      if d <= (0 :: Int) then pure [nodeName n]
      else do
        kids <- calleesOf n <&> filter (keep anc)
        let kids' = nubBy (\a b -> nodeName a == nodeName b) kids
        if null kids' then pure [nodeName n]      -- LEAF
        else concatMapM (go (d - 1) (nodeName n : anc)) kids'


-- ===== MODEL-DRIVEN INVESTIGATION: lspSeek ===========================
-- fable's `seek` ported to LSP edges, driven by the local model via a DYNAMIC
-- structured-output move schema (the model picks op + target-by-semantic-key,
-- constrained to the live registry). ADAPTIVE + fuel-bounded → finds a/THE X,
-- NOT every X (lossy by design; for enumeration use the deterministic closure).
--
-- The three GUARDS are the real IP (fable's v1→v5 arc, reproduced here):
--   (a) seed-guard: an empty registry forces a `where` first.
--   (b) reject-premature-done: can't conclude on only the seed (must expand).
--   (c) cite-evidence: the answer must name a symbol actually in the registry.
-- Without them the model hallucinated ("allocate_bump"); WITH them it traced
-- value_to_heap → callees → bump_alloc_from_vmctx correctly (6 probes, autonomous).
-- TODO for the lib version: add fable's DUPLICATE-MOVE guard (the model repeated
-- `callees 5`, wasting a probe), and parameterize the driver (tryLlm vs ask, so a
-- human can drive the same schema).

mkKey :: Int -> Node -> Text
mkKey i n = show i <> ": " <> (if T.null (nodeContainer n) then "" else nodeContainer n <> "::") <> nodeName n

lspSeek :: Text -> Int -> M Value
lspSeek goal fuel0 = loopM step (fuel0, [], [])
  where
    keepN n = isPrefixOf "tidepool" (nodeFile n) && (nodeKind n == "function" || nodeKind n == "method")
    regNames reg = map (nodeName . snd) reg
    header trail reg =
      "GOAL: " <> goal
      <> "\n\nRegistry (target = exact key; answer must be one of these names):\n"
      <> intercalate "\n" (map (\(k, n) -> k <> " [" <> nodeKind n <> "]") (take 30 reg))
      <> "\n\nRecent findings:\n" <> (if null trail then "(none)" else intercalate "\n---\n" (take 3 trail))
    digest lbl ns reg =
      let base  = length reg
          keyed = imap (\i n -> (mkKey (base + i) n, n)) ns
          ls    = map (\(k, n) -> k <> " in " <> nodeFile n) (take 25 keyed)
      in (lbl <> " → " <> show (length ns) <> ":\n" <> intercalate "\n" ls, reg ++ keyed)
    runOp lbl tgt reg trail k f =
      case lookup tgt reg of
        Just n  -> do { rs <- f n <&> filter keepN
                      ; let (d, reg') = digest (lbl <> " " <> tgt) rs reg
                        in pure (Right (k - 1, d : trail, reg')) }
        Nothing -> pure (Right (k - 1, ("REJECTED bad target '" <> tgt <> "' — pick an exact registry key") : trail, reg))
    done k reg trail arg rsn = Left (object ["answer" .= arg, "reason" .= rsn, "probes_used" .= (fuel0 - k), "trail" .= reverse (take 10 trail)])
    step (k, trail, reg)
      | null reg = do                                 -- (a) SEED GUARD
          v <- tryLlm (SObj [("reason", SStr), ("seed_name", SStr)])
                 ("GOAL: " <> goal <> "\n\nSeed: name ONE symbol to look up first.")
          case v of
            Left e   -> pure (done k reg trail "" ("seed error: " <> T.take 80 e))
            Right vv -> do
              ns <- lspWhere (maybe "" id (vv ^? key "seed_name" . _String)) <&> filter keepN
              let (d, reg') = digest "seed" ns reg
              pure (Right (k - 1, d : trail, reg'))
      | otherwise = do
          let sch = SObj [ ("reason", SStr), ("op", SEnum ["callers","callees","refs","hover","done"])
                         , ("target", SEnum (map fst reg)), ("answer", SStr) ]
          mv <- tryLlm sch (header trail reg <> "\n\nFuel " <> show k <> ". Next move. Only 'done' when the answer is a registered symbol you reached by walking.")
          case mv of
            Left e  -> pure (done k reg trail "" ("model error: " <> T.take 80 e))
            Right v -> do
              let g f = maybe "" id (v ^? key f . _String)
                  op = g "op"; tgt = g "target"; rsn = g "reason"; ans = g "answer"
              if op == "done"
                then if length reg <= 1                                       -- (b) no premature done
                       then pure (Right (k - 1, "REJECTED done: you only seeded — walk callees/callers first." : trail, reg))
                       else if not (elem ans (regNames reg))                  -- (c) cite evidence
                         then pure (Right (k - 1, ("REJECTED done: answer '" <> ans <> "' is not a symbol you found.") : trail, reg))
                         else pure (done k reg trail ans rsn)
                else if k <= (1 :: Int) then pure (done k reg trail ans rsn)
                else case op of
                  "callers" -> runOp "callers" tgt reg trail k (\n -> lspCallers n <&> fromMaybe [])
                  "callees" -> runOp "callees" tgt reg trail k (\n -> lspCallees n <&> fromMaybe [])
                  "refs"    -> runOp "refs" tgt reg trail k (\n -> lspRefs n <&> fromMaybe [])
                  "hover"   -> case lookup tgt reg of
                                 Just n  -> lspHover n <&> \h -> Right (k - 1, ("hover " <> tgt <> ":\n" <> maybe "(none)" (T.take 200) h) : trail, reg)
                                 Nothing -> pure (Right (k - 1, ("bad target " <> tgt) : trail, reg))
                  _ -> pure (Right (k - 1, ("unknown op " <> op) : trail, reg))


-- ===== where heuristic-gated LLM should go (NOT into the edge-walk) ===
-- The deterministic closure is the skeleton; the model enters at the JOINTS,
-- steer-gated (rule first, model only when rules abstain), preserving completeness:
--   1. PREDICATE-as-cascade: replace `isUnsafe` (text) with a `steer` — pure rule
--      on name/kind/text, escalate to model (with hover context) only for ambiguous
--      nodes. Every node still VISITED; only classification gets semantic. (headline)
--   2. SEED resolution: `the name intent` to resolve a fuzzy entry to the right Node.
--   3. RESULT synthesis: fold the COMPLETE hit-set through the model (rank/cluster/
--      explain) — model summarizes output, never gates the search.
--   4. FRONTIER prioritization: only when the cone is too big to fixpoint, model-
--      scored best-first (trades completeness for focus, transparently).
-- Rule: the model JUDGES (predicate/seed/synthesis/priority); the scheme TRAVERSES.
-- lspSeek is the deliberate inversion (model owns the walk) — right for "find a path",
-- wrong for "find every X".
