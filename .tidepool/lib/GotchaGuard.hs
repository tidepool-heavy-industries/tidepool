{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
-- | Dogfood: Core-translation gotcha coverage map.
--
-- The gotchas are documented in docs/core-shapes/audit-translate.md. This module
-- checks whether each documented gotcha has a corresponding handler somewhere in
-- the Haskell/Rust source, and flags gaps. Deterministic now; the LLM judgment
-- layer ("is this gap real or is the handler named differently?") comes later.
-- Core lives here; eval is a thin shell (`gotchaReport`, `gotchaDriftReport`).
module GotchaGuard where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import qualified Data.Text as T
import qualified Data.Set as Set

-- | A documented translation gotcha.
data Gotcha = Gotcha
  { gName        :: Text
  , gDescription :: Text
  , gPatterns    :: [Text]   -- search terms in source
  , gDocAliases  :: [Text]   -- matching headings in audit-translate.md
  }

-- | Constructor with no doc aliases (the common case).
gotcha :: Text -> Text -> [Text] -> Gotcha
gotcha n d p = Gotcha n d p []

-- | Where a pattern matched.
data Hit = Hit
  { hFile :: Text
  , hLine :: Int
  , hText :: Text
  }

-- | Catalog copied from docs/core-shapes/audit-translate.md.
-- Names are stable ids; descriptions are short reminders; patterns are the
-- terms most likely to appear in handler code. Patterns are searched one at a
-- time so we don't have to build a correct OR-regex over strings containing
-- regex metacharacters.
catalog :: [Gotcha]
catalog =
  [ gotcha "tagToEnum# desugar"
      "Magical primop desugared to Case on constructor tags."
      ["TagToEnum", "tagToEnum"]
  , Gotcha "joinrec -> LetRec promotion"
      "Recursive join points promoted to regular lambdas."
      ["joinrec", "joinIdToRec", "RecJoinIds"]
      ["joinrec → LetRec promotion"]
  , Gotcha "jumpCrossesLam"
      "Join points used inside lambdas promoted to closures."
      ["jumpCrossesLam"]
      ["jumpCrossesLam (Join Point conversion)"]
  , Gotcha "EqSpec arity adjustment"
      "GADT constructor arity adjusted for erased equality evidence."
      ["EqSpec", "valueRepArity"]
      ["valueRepArity"]
  , Gotcha "unsafeEqualityProof elision"
      "GADT equality proof cases elided to unit evidence."
      ["unsafeEqualityProof", "UnsafeRefl"]
      ["isUnsafeEqualityCase elision", "isUnsafeEqualityProofVar desugar"]
  , Gotcha "runRW# state-token erasure"
      "runRW# state token erased for pure JIT execution."
      ["runRW", "isRunRWVar"]
      ["isRunRWVar desugar"]
  , Gotcha "realWorld# erasure"
      "RealWorld state token replaced with dummy literal."
      ["realWorld", "isRealWorldVar"]
      ["isRealWorldVar"]
  , Gotcha "type metadata poison"
      "Typeable metadata vars emitted as error nodes."
      ["trModule", "krep", "tcType"]
      ["isTypeMetadataVar"]
  , Gotcha "stateful primop State# drop"
      "State# tokens stripped from stateful primops."
      ["stateful primop", "State# RealWorld"]
      ["State# token argument drop", "Stateful primop state erasure"]
  , Gotcha "unboxed tuple handling"
      "Unboxed tuples lowered to heap Cons or passthrough."
      ["unboxed tuple"]
      ["isUnboxedTupleDataCon (general)"]
  , gotcha "DataCon wrapper canonicalization"
      "DataCon wrapper Ids canonicalized to worker Ids."
      ["dataConWrapId", "dataConWorkId"]
  , gotcha "localVarId hash disambiguation"
      "Local VarIds hashed with OccName to avoid collisions."
      ["localVarId", "OccName"]
  , Gotcha "stableVarId for externals"
      "External Names get stable hashed IDs."
      ["stableVarId", "ModuleName"]
      ["stableVarId"]
  , gotcha "isErrorVar intercept"
      "error/undefined/patError converted to JIT error nodes."
      ["isErrorVar", "divZeroError", "overflowError"]
  , Gotcha "unpackCString# static desugar"
      "Static string literals desugared to Cons chains."
      ["unpackCString", "isUnpackCStringVar"]
      ["isUnpackCStringVar static desugar"]

  -- Extended from audit-translate.md headings not already covered above.
  , gotcha "emitRuntimeUnpackCString"
      "Runtime fallback for dynamic unpackCString# addr."
      ["emitRuntimeUnpackCString"]
  , gotcha "emitRuntimeUnpackAppendCString"
      "Runtime fallback for dynamic unpackAppendCString# addr suffix."
      ["emitRuntimeUnpackAppendCString"]
  , gotcha "emitShowDoubleSpecBody"
      "Specialized Double show pipeline intercepted."
      ["emitShowDoubleSpecBody", "ShowDoubleAddr"]
  , gotcha "reachableBinds"
      "Reachability filter to avoid resolving whole modules."
      ["reachableBinds"]
  , gotcha "isShowDoubleVar intercept"
      "Direct showDouble Var intercepted."
      ["isShowDoubleVar"]
  , gotcha "isShowDoubleSpecVar intercept"
      "Specialized show Double Var intercepted."
      ["isShowDoubleSpecVar"]
  , gotcha "isUnpackCStringVar dynamic fallback"
      "Dynamic unpackCString# addr routed to runtime."
      ["isUnpackCStringVar"]
  , gotcha "isUnpackAppendCStringVar dynamic fallback"
      "Dynamic unpackAppendCString# routed to runtime."
      ["isUnpackAppendCStringVar"]
  , gotcha "isUnpackAppendCStringVar partial/eta-reduced"
      "Eta-reduced unpackAppendCString# handled."
      ["isUnpackAppendCStringVar"]
  , gotcha "isUnpackAppendCStringVar static prefix desugar"
      "Static prefix unpackAppendCString# desugared."
      ["isUnpackAppendCStringVar"]
  , gotcha "isUnpackFoldrCStringVar static desugar"
      "Static unpackFoldrCString# desugared."
      ["isUnpackFoldrCStringVar"]
  , gotcha "isAppendVar desugar"
      "Var (++) desugared to recursive loop."
      ["isAppendVar"]
  , gotcha "isUnsafeTakeVar desugar"
      "unsafeTake worker-wrapper desugared."
      ["isUnsafeTakeVar"]
  , gotcha "isRuntimeErrorVar / isErrorVar handling"
      "Runtime error sentinels converted to error nodes."
      ["isRuntimeErrorVar", "isErrorVar"]
  , gotcha "splitMultiReturnPrimOp desugar"
      "quotRem/timesInt2 multi-return split to single-return."
      ["splitMultiReturnPrimOp"]
  , gotcha "Coercion placeholder"
      "Coercion in expression position replaced with dummy."
      ["Coercion", "isCoercion"]
  , gotcha "FFI primop support"
      "FCall-wrapped primops routed through FFI desugar."
      ["isFCallId", "FFI"]
  , gotcha "Multi-element unboxed tuples"
      "Multi-element unboxed tuples heap-allocated."
      ["unboxed tuple"]
  , Gotcha "Unresolved external Id -> kind-4 error node"
      "Missing external Ids become runtime poison."
      ["unresolved", "NError 4"]
      ["Unresolved external Id → kind-4 error node"]
  ]

-- | Search for one pattern; return matches in source files.
searchPattern :: Text -> Text -> M [Hit]
searchPattern glob pat = do
  hits <- grepGlob pat glob
  pure [ Hit f l t | (f, l, t) <- hits ]

-- | Find handlers for one gotcha across both Haskell and Rust source.
-- Each pattern is searched separately and hits are unioned.
findHandlers :: Gotcha -> M (Gotcha, [Hit])
findHandlers g = do
  rsHits <- concatMapM (searchPattern "**/*.rs") (gPatterns g)
  hsHits <- concatMapM (searchPattern "haskell/**/*.hs") (gPatterns g)
  let allHits = nubBy sameLoc (rsHits ++ hsHits)
  pure (g, allHits)
  where
    sameLoc a b = hFile a == hFile b && hLine a == hLine b

-- | Deterministic report: gotcha -> handler count + sample locations.
gotchaReport :: M Value
gotchaReport = do
  results <- mapM findHandlers catalog
  let entries = map mkEntry results
      mkEntry (g, hits) = object
        [ "name"        .= gName g
        , "description" .= gDescription g
        , "handlers"    .= len hits
        , "samples"     .= map (\h -> object ["file" .= hFile h, "line" .= hLine h, "text" .= T.strip (hText h)]) (take 3 hits)
        , "gap"         .= (len hits == 0)
        ]
      gaps = filter (\v -> Just True == (v ^? key "gap" . _Bool)) entries
  pure (object
    [ "total"  .= len catalog
    , "gaps"   .= len gaps
    , "gapped" .= map (\v -> fromMaybe "?" (v ^? key "name" . _String)) gaps
    , "details".= entries
    ])

-- | Parse the documented gotcha names out of audit-translate.md.
-- Each '## <Name>' heading becomes a gotcha id.  We strip trailing
-- parenthetical locations and normalize lightly.
docGotchas :: M [Text]
docGotchas = do
  txt <- readFile "docs/core-shapes/audit-translate.md"
  let heading l = case T.stripPrefix "## " (T.strip l) of
        Just rest -> Just (T.strip rest)
        Nothing   -> Nothing
  pure (mapMaybe heading (T.lines txt))

-- | All names by which a catalog entry is known (id + doc aliases).
catalogKeys :: Gotcha -> [Text]
catalogKeys g = gName g : gDocAliases g

-- | Compare the in-code catalog against the markdown doc and the source tree.
-- Flags three kinds of drift:
--   1. documented but not in catalog (doc outran code)
--   2. catalogged but not in doc     (catalog stale)
--   3. catalogged but no handlers    (real coverage gap)
gotchaDriftReport :: M Value
gotchaDriftReport = do
  docNames      <- docGotchas
  results       <- mapM findHandlers catalog
  let docSet    = Set.fromList docNames
      catSet    = Set.fromList (concatMap catalogKeys catalog)
      documentedNotCatalog = Set.toList (docSet `Set.difference` catSet)
      catalogNotDocumented =
        [ gName g | g <- catalog, Set.null (Set.intersection (Set.fromList (catalogKeys g)) docSet) ]
      gapped    = [ gName g | (g, hits) <- results, null hits ]
  pure (object
    [ "documented_total"       .= len docNames
    , "catalog_total"          .= len catalog
    , "documented_not_catalog" .= documentedNotCatalog
    , "catalog_not_documented" .= catalogNotDocumented
    , "handler_gaps"           .= gapped
    , "ok"                     .= (null documentedNotCatalog && null catalogNotDocumented && null gapped)
    ])
