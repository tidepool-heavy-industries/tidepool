module Tidepool.Resolve (resolveExternals, UnresolvedVar(..)) where

import GHC.Core (CoreBind, CoreExpr, Bind(..), Expr(..), maybeUnfoldingTemplate)
import GHC.Core.FVs (exprSomeFreeVars)
import GHC.Types.Id (Id, idUnfolding, realIdUnfolding, isGlobalId, isPrimOpId_maybe, isDataConWorkId_maybe, isDataConWrapId_maybe)
import GHC.Types.Var (Var, varName, varUnique)
import GHC.Types.Var.Set (VarSet, emptyVarSet, unitVarSet, elemVarSet, extendVarSet)
import GHC.Types.Unique (getKey)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Types.Name (nameOccName, nameModule_maybe)
import GHC.Types.Name.Occurrence (occNameString, mkVarOcc)
import GHC.Unit.Module (moduleName, moduleNameString)
import Data.Word (Word64)
import Data.Char (isDigit)
import Data.List (isPrefixOf, isInfixOf)
import qualified Data.Map.Strict as Map

-- For specialization fallback
import GHC.Driver.Env (HscEnv(..), hscEPS)
import GHC.Unit.External (ExternalPackageState(..))
import GHC.Types.Name.Cache (nsNames, lookupOrigNameCache)
import GHC.Types.Name.Env (lookupNameEnv)
import GHC.Types.TyThing (TyThing(..))
import Control.Concurrent.MVar (readMVar)

-- Fat interface fallback (mi_extra_decls) — disabled for now, threshold bump
-- may be sufficient. Re-enable if workers still return NoUnfolding.
-- import Tidepool.FatIface (FatIfaceCache, newFatIfaceCache, lookupFatIface)

data UnresolvedVar = UnresolvedVar
  { uvKey    :: !Word64
  , uvName   :: !String
  , uvModule :: !String
  } deriving (Show)

-- | Resolve cross-module references by inlining their unfoldings.
--
-- Uses exprSomeFreeVars (const True) instead of exprFreeVars because
-- the latter filters to isLocalVar, excluding all GlobalIds. Since we
-- serialize Core into a self-contained CBOR tree (no linker), we need
-- to discover and inline global references too.
--
-- When a variable's unfolding is missing, attempts specialization
-- fallback: parses the OccName for $s markers, derives the generic
-- parent name, and looks it up via the HscEnv's NameCache and EPS.
-- If that also fails (e.g., for $fOrdList_$ccompare which is a
-- record selector with no unfolding), falls back to Prelude substitutes
-- for known patterns (compareString for Ord list compare, eqString
-- for Eq list ==).
--
-- Returns: (augmented bindings, list of variables that could not be resolved).
resolveExternals :: HscEnv -> [CoreBind] -> IO ([CoreBind], [UnresolvedVar])
resolveExternals hscEnv binds = do
  let localBinders = foldMap bindersOfSet binds
      allFreeVars  = foldMap freeVarsOfBind binds
      externals    = filter (isResolvable localBinders) (nonDetEltsUniqSet allFreeVars)
      -- Build name→Var lookup for Prelude substitution fallback
      localNameMap = buildLocalNameMap binds
  (resolvedBinds, substituteBinds, _, unresolved) <- go localNameMap externals emptyVarSet localBinders [] [] []
  let resolvedPairs = concatMap toRecPairs resolvedBinds
      substitutePairs = concatMap toRecPairs substituteBinds
      -- All three groups (resolved, originals, substitutes) form a 3-way cycle:
      -- originals → resolved (via GHC unfoldings), resolved → substitutes (via
      -- specialized method refs), substitutes → originals (via Var aliases).
      -- Merge into one Rec so the JIT's LetRec handling can pre-allocate all
      -- bindings before compiling any Lam bodies. The reachableBinds filter in
      -- Translate.hs handles individual binding reachability within the Rec.
      origPairs = concatMap toRecPairs binds
      allPairs = resolvedPairs ++ origPairs ++ substitutePairs
      augmented = if null allPairs then [] else [Rec allPairs]
  return (augmented, unresolved)
  where
    go :: Map.Map String (Var, CoreExpr) -> [Var] -> VarSet -> VarSet
       -> [CoreBind] -> [CoreBind] -> [UnresolvedVar]
       -> IO ([CoreBind], [CoreBind], VarSet, [UnresolvedVar])
    go _ [] visited _ acc subAcc unres = return (reverse acc, reverse subAcc, visited, reverse unres)
    go nameMap (v:rest) visited localSet acc subAcc unres
      | elemVarSet v visited = go nameMap rest visited localSet acc subAcc unres
      | otherwise = do
          let visited' = extendVarSet visited v
              vName = occNameString (nameOccName (varName v))
              handleUnfolding unfoldingExpr =
                let newBind = NonRec v unfoldingExpr
                    newFVs = exprSomeFreeVars (const True) unfoldingExpr
                    localSet' = extendVarSet localSet v
                    newExternals = filter (isResolvable localSet')
                                         (nonDetEltsUniqSet newFVs)
                in go nameMap (newExternals ++ rest) visited' localSet' (newBind : acc) subAcc unres
          case maybeUnfoldingTemplate (idUnfolding v) of
               Just unfoldingExpr -> handleUnfolding unfoldingExpr
               Nothing -> case maybeUnfoldingTemplate (realIdUnfolding v) of
                 Just unfoldingExpr -> handleUnfolding unfoldingExpr
                 Nothing -> do
                   -- Standard unfolding failed. Attempt specialization fallback.
                   mbFallback <- attemptSpecFallback hscEnv v
                   case mbFallback of
                     Just (genId, unfoldingExpr) ->
                       let genBind = NonRec genId unfoldingExpr
                           aliasBind = NonRec v (Var genId)  -- alias $s var → generic parent
                           newFVs = exprSomeFreeVars (const True) unfoldingExpr
                           localSet' = extendVarSet (extendVarSet localSet v) genId
                           newExternals = filter (isResolvable localSet')
                                                 (nonDetEltsUniqSet newFVs)
                       in go nameMap (newExternals ++ rest) visited' localSet' (genBind : acc) (aliasBind : subAcc) unres
                     Nothing ->
                       -- Despec failed too. Try Prelude substitution.
                       case preludeSubstitute nameMap vName v of
                         Just subBind ->
                           let localSet' = extendVarSet localSet v
                           in go nameMap rest visited' localSet' acc (subBind : subAcc) unres
                         Nothing ->
                           let uv = UnresolvedVar
                                 { uvKey    = fromIntegral (getKey (varUnique v))
                                 , uvName   = occNameString (nameOccName (varName v))
                                 , uvModule = case nameModule_maybe (varName v) of
                                                Just m  -> moduleNameString (moduleName m)
                                                Nothing -> "<no module>"
                                 }
                           in go nameMap rest visited' localSet acc subAcc (uv : unres)

    toRecPairs :: CoreBind -> [(Var, CoreExpr)]
    toRecPairs (NonRec b rhs) = [(b, rhs)]
    toRecPairs (Rec pairs)    = pairs

    isResolvable :: VarSet -> Var -> Bool
    isResolvable localSet v =
      isGlobalId v
      && not (elemVarSet v localSet)
      && not (isPrimOp v)
      && not (isDataCon v)
      && not (isMagicUnpackVar v)

    bindersOfSet :: CoreBind -> VarSet
    bindersOfSet (NonRec b _) = unitVarSet b
    bindersOfSet (Rec pairs) = foldl (\s (b, _) -> extendVarSet s b) emptyVarSet pairs

    freeVarsOfBind :: CoreBind -> VarSet
    freeVarsOfBind (NonRec _ rhs) = exprSomeFreeVars (const True) rhs
    freeVarsOfBind (Rec pairs) = foldMap (exprSomeFreeVars (const True) . snd) pairs

    isPrimOp :: Id -> Bool
    isPrimOp v = case isPrimOpId_maybe v of
      Just _  -> True
      Nothing -> False

    isDataCon :: Id -> Bool
    isDataCon v = case isDataConWorkId_maybe v of
      Just _  -> True
      Nothing -> case isDataConWrapId_maybe v of
        Just _  -> True
        Nothing -> False

    -- | Skip magic unpack/string functions that Translate.hs handles specially.
    -- Their unfoldings use Addr# primops (plusAddr#, indexCharOffAddr#) that we
    -- don't support; instead, Translate desugars them to cons-cell chains.
    isMagicUnpackVar :: Var -> Bool
    isMagicUnpackVar v =
      let name = occNameString (nameOccName (varName v))
      in name `elem` [ "unpackCString#", "unpackCStringUtf8#"
                      , "unpackAppendCString#"
                      , "unpackFoldrCString#", "unpackFoldrCStringUtf8#" ]

-- | Build a map from OccName string to (Var, CoreExpr) for all local binders.
-- Used for Prelude substitution: when a specialized method can't be resolved,
-- we inline the Prelude function's RHS directly.
buildLocalNameMap :: [CoreBind] -> Map.Map String (Var, CoreExpr)
buildLocalNameMap = foldl addBind Map.empty
  where
    addBind m (NonRec b rhs) = Map.insert (occNameString (nameOccName (varName b))) (b, rhs) m
    addBind m (Rec pairs)  = foldl (\m' (b, rhs) -> Map.insert (occNameString (nameOccName (varName b))) (b, rhs) m') m pairs

-- | Known mappings from GHC specialized typeclass method patterns to
-- Prelude function names. When a specialized method like
-- $fOrdList_$s$ccompare1 can't be resolved (and neither can its generic
-- form), we substitute with our Prelude's equivalent.
--
-- Pattern: if the OccName contains the method pattern, use the substitute.
preludeMethodSubstitutes :: [(String, String)]
preludeMethodSubstitutes =
  [ ("$fOrdList_$s$ccompare", "compareString")  -- Ord [a] compare → compareString
  , ("$fOrdList_$ccompare",   "compareString")  -- generic also fails
  , ("$fEqList_$s$c==",       "eqString")       -- Eq [a] == → eqString
  , ("$fEqList_$c==",         "eqString")       -- generic also fails
  , ("eqString",              "eqString")       -- GHC.Internal.Base.eqString (RULE rewrite)
  ]

-- | Try to substitute an unresolvable specialized var with a Prelude function.
-- Creates a simple alias (NonRec specVar (Var preludeVar)) pointing to the
-- local Prelude function. The alias binding is placed in the same Rec group
-- as originals so both can see each other.
preludeSubstitute :: Map.Map String (Var, CoreExpr) -> String -> Var -> Maybe CoreBind
preludeSubstitute nameMap specName specVar =
  case findSubstitute specName of
    Nothing -> Nothing
    Just preludeName ->
      case Map.lookup preludeName nameMap of
        Nothing -> Nothing
        Just (preludeVar, _preludeRhs) ->
          Just (NonRec specVar (Var preludeVar))
  where
    findSubstitute :: String -> Maybe String
    findSubstitute name = go preludeMethodSubstitutes
      where
        go [] = Nothing
        go ((pat, sub):rest)
          | pat `isPrefixOf` name = Just sub
          | otherwise = go rest

-- | Attempt to resolve a specialized Id by deriving its generic parent.
-- Parses the OccName for $s markers, strips them to get the generic name,
-- then looks up the generic Id in the same module via NameCache + EPS.
attemptSpecFallback :: HscEnv -> Var -> IO (Maybe (Id, CoreExpr))
attemptSpecFallback hscEnv specVar = do
  let occStr = occNameString (nameOccName (varName specVar))
  case despecializeOccName occStr of
    Nothing -> return Nothing
    Just genOccStr -> do
      case nameModule_maybe (varName specVar) of
        Nothing -> return Nothing
        Just modCtx -> do
          let nc = hsc_NC hscEnv
          origNc <- readMVar (nsNames nc)
          let genOcc = mkVarOcc genOccStr
          case lookupOrigNameCache origNc modCtx genOcc of
            Nothing -> return Nothing
            Just genName -> do
              eps <- hscEPS hscEnv
              let pte = eps_PTE eps
              case lookupNameEnv pte genName of
                Just (AnId genId) ->
                  case maybeUnfoldingTemplate (realIdUnfolding genId) of
                    Just expr -> return (Just (genId, expr))
                    Nothing ->
                      case maybeUnfoldingTemplate (idUnfolding genId) of
                        Just expr -> return (Just (genId, expr))
                        Nothing -> return Nothing
                _ -> return Nothing

-- | Strip GHC specialization markers from an OccName string.
-- Returns Just the generic name, or Nothing if no $s markers found.
--
-- GHC naming patterns:
--   Method specialization: "$fOrdList_$s$ccompare1" -> "$fOrdList_$ccompare"
--     (suffix after _$s starts with $c = method marker, keep it)
--   Operator specialization: "$fEqList_$s$c==1" -> "$fEqList_$c=="
--   Dict self-specialization: "$fOrdList_$s$fOrdList1" -> "$fOrdList"
--     (suffix after _$s starts with $f = dict self-ref, drop entire suffix)
--   Worker specialization: "$fOrdList_$s$w$ccompare" -> "$fOrdList_$w$ccompare"
--   Top-level specialization: "$sshow1" -> "show"
despecializeOccName :: String -> Maybe String
despecializeOccName occStr
  | "_$s" `isInfixOf` occStr = Just $ despecInfix occStr
  | "$s" `isPrefixOf` occStr = Just $ cleanTrailingDigits $ drop 2 occStr
  | otherwise = Nothing
  where
    cleanTrailingDigits :: String -> String
    cleanTrailingDigits s =
      let r = reverse s
      in case r of
           [] -> s
           (c:_) | isDigit c -> reverse (dropWhile isDigit r)
           _ -> s

    -- Split at "_$s" and handle based on suffix type
    despecInfix :: String -> String
    despecInfix s =
      let (prefix, suffix) = splitAtSpec s
      in case suffix of
           -- Method marker ($c) or worker ($w): reattach as _$c... or _$w...
           ('$':'c':_) -> cleanTrailingDigits (prefix ++ "_" ++ suffix)
           ('$':'w':_) -> cleanTrailingDigits (prefix ++ "_" ++ suffix)
           -- Dict self-reference ($f) or anything else: prefix is the generic name
           _           -> cleanTrailingDigits prefix

    -- Split "foo_$sbar" into ("foo", "bar") at the first "_$s"
    splitAtSpec :: String -> (String, String)
    splitAtSpec s = go [] s
      where
        go acc [] = (reverse acc, [])
        go acc rest@(c:_)
          | "_$s" `isPrefixOf` rest = (reverse acc, drop 3 rest)
          | otherwise = go (c : acc) (drop 1 rest)
