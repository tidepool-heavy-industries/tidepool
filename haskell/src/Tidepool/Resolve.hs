module Tidepool.Resolve (resolveExternals, UnresolvedVar(..)) where

import GHC.Core (CoreBind, CoreExpr, Bind(..), Expr(..), Alt(..), maybeUnfoldingTemplate)
import GHC.Core.FVs (exprSomeFreeVars)
import GHC.Core.Subst (substExpr, mkEmptySubst)
import GHC.Types.Var.Env (mkInScopeSet)
import GHC.Types.Id (Id, idType, idUnfolding, realIdUnfolding, isGlobalId, isPrimOpId_maybe, isDataConWorkId_maybe, isDataConWrapId_maybe, isDeadEndId, mkSysLocalOrCoVar)
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
import Data.Maybe (catMaybes)

-- For specialization fallback
import GHC.Driver.Env (HscEnv(..), hscEPS)
import GHC.Unit.External (ExternalPackageState(..))
import GHC.Types.Name.Cache (nsNames, lookupOrigNameCache)
import GHC.Types.Name.Env (lookupNameEnv)
import GHC.Types.TyThing (TyThing(..))
import Control.Concurrent.MVar (readMVar)

-- For dictionary reconstruction (spec-fallback arity-mismatch repair).
-- GHC's SPEC rules rewrite e.g. NE.group/groupBy to a base binding whose
-- Foldable dictionary value-arg was specialized away, shipped with no
-- unfolding. Despecializing to a BARE ALIAS of the generic (which still
-- takes the dict) under-saturates it → partial application → case trap.
-- We instead reconstruct the concrete dictionary and eta-expand.
import GHC.Core.InstEnv (InstEnvs(..), lookupInstEnv, instanceDFunId, emptyInstEnv)
import GHC.Core.Make (mkCoreLams, mkCoreApps)
import GHC.Core.Predicate (getClassPredTys_maybe)
import GHC.Core.Type (substTy, mkTyVarTy)
import GHC.Core.Unify (tcMatchTy)
import GHC.Core.Multiplicity (scaledThing)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy, tcSplitFunTys)
import GHC.Builtin.Types (manyDataConTy)
import GHC.Types.Unique.Supply (UniqSupply, mkSplitUniqSupply, uniqsFromSupply)
import GHC.Unit.Module.Env (emptyModuleSet)
import GHC.Data.FastString (fsLit)

-- Fat interface fallback (mi_extra_decls) — for loop-breakers whose
-- unfoldings are not exposed via realIdUnfolding even with threshold bumps.
import Tidepool.FatIface (FatIfaceCache, newFatIfaceCache, lookupFatIface)

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
-- If that also fails, falls back to fat interface lookup
-- (mi_extra_decls) which handles loop-breakers and typeclass methods
-- whose unfoldings aren't exposed via realIdUnfolding.
--
-- Returns: (augmented bindings, list of variables that could not be resolved).
-- | @varIdFn@ is Translate's stable @varId@: the unresolved-var KEY must be in
-- the SAME key space the translator looks them up by (its NVar varIds), NOT the
-- raw GHC varUnique — otherwise the guard silently never fires and unresolved
-- externals leak through as live NVars instead of poison error nodes.
resolveExternals :: (Var -> Word64) -> HscEnv -> [CoreBind] -> IO ([CoreBind], [UnresolvedVar])
resolveExternals varIdFn hscEnv binds = do
  let localBinders = foldMap bindersOfSet binds
      allFreeVars  = foldMap freeVarsOfBind binds
      allFVList    = nonDetEltsUniqSet allFreeVars
      externals    = filter (isResolvable localBinders) allFVList
  fatCache <- newFatIfaceCache
  (resolvedBinds, substituteBinds, _, unresolved) <- go fatCache externals emptyVarSet localBinders [] [] []
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
    go :: FatIfaceCache -> [Var] -> VarSet -> VarSet
       -> [CoreBind] -> [CoreBind] -> [UnresolvedVar]
       -> IO ([CoreBind], [CoreBind], VarSet, [UnresolvedVar])
    go _ [] visited _ acc subAcc unres = return (reverse acc, reverse subAcc, visited, reverse unres)
    go fatCache (v:rest) visited localSet acc subAcc unres
      | elemVarSet v visited = go fatCache rest visited localSet acc subAcc unres
      -- Over-collected modules (e.g. GHC.Fingerprint's MD5 machinery, dragged into
      -- rationalToDouble's closure) — record as unresolved -> lazy poison instead of
      -- pulling their (always-dead-here) MD5/Int32 bodies into the table.
      | isNeverResolve v =
          let uv = UnresolvedVar
                { uvKey    = varIdFn v
                , uvName   = occNameString (nameOccName (varName v))
                , uvModule = case nameModule_maybe (varName v) of
                               Just m  -> moduleNameString (moduleName m)
                               Nothing -> "<no module>"
                }
          in go fatCache rest (extendVarSet visited v) localSet acc subAcc (uv : unres)
      | otherwise = do
          let visited' = extendVarSet visited v
          let handleUnfolding unfoldingExpr =
                let renamedExpr = alphaRenameExpr localSet unfoldingExpr
                    newBind = NonRec v renamedExpr
                    newFVs = exprSomeFreeVars (const True) renamedExpr
                    internalBinders = collectLocalBinders renamedExpr
                    localSet' = foldl extendVarSet (extendVarSet localSet v) internalBinders
                    newExternals = filter (isResolvable localSet')
                                         (nonDetEltsUniqSet newFVs)
                in go fatCache (newExternals ++ rest) visited' localSet' (newBind : acc) subAcc unres
          case maybeUnfoldingTemplate (idUnfolding v) of
               Just unfoldingExpr -> handleUnfolding unfoldingExpr
               Nothing -> case maybeUnfoldingTemplate (realIdUnfolding v) of
                 Just unfoldingExpr -> handleUnfolding unfoldingExpr
                 Nothing -> do
                   -- Standard unfolding failed. Attempt specialization fallback.
                   mbFallback <- attemptSpecFallback hscEnv v
                   case mbFallback of
                     Just (genId, unfoldingExpr, aliasRhs) ->
                       let renamedExpr = alphaRenameExpr localSet unfoldingExpr
                           genBind = NonRec genId renamedExpr
                           -- aliasRhs is either a bare `Var genId` or a
                           -- dict-reconstructing wrapper. The wrapper introduces
                           -- a NEW external (the instance dfun), so collect its
                           -- free vars too — otherwise the dfun never resolves.
                           aliasBind = NonRec v aliasRhs
                           newFVs = exprSomeFreeVars (const True) renamedExpr
                           aliasFVs = exprSomeFreeVars (const True) aliasRhs
                           internalBinders = collectLocalBinders renamedExpr
                           localSet' = foldl extendVarSet (extendVarSet (extendVarSet localSet v) genId) internalBinders
                           newExternals = filter (isResolvable localSet')
                                                 (nonDetEltsUniqSet newFVs ++ nonDetEltsUniqSet aliasFVs)
                       in go fatCache (newExternals ++ rest) visited' localSet' (genBind : acc) (aliasBind : subAcc) unres
                     Nothing -> do
                           -- Despec failed too. Fat interface fallback (mi_extra_decls).
                           -- Handles loop-breakers like itos' whose unfoldings are
                           -- not exposed via realIdUnfolding.
                           mbFat <- lookupFatIface hscEnv fatCache (varName v)
                           case mbFat of
                             Just (NonRec _b fatExpr) -> handleUnfolding fatExpr
                             Just (Rec fatPairs) ->
                               -- Pull the ENTIRE Rec group, not just the requested
                               -- binding. Rec groups from fat interfaces may contain
                               -- join points that siblings reference. Without all
                               -- members, join point definitions are missing and the
                               -- JIT emits "Jump to unknown label JoinId(...)".
                               let binders = [b | (b, _) <- fatPairs]
                                   localSetWithBinders = foldl extendVarSet localSet binders
                                   renamedPairs = [(b, alphaRenameExpr localSetWithBinders e) | (b, e) <- fatPairs]
                                   fatBinds = [NonRec b e | (b, e) <- renamedPairs]
                                   allFVs = foldMap (exprSomeFreeVars (const True) . snd) renamedPairs
                                   allInternalBinders = concatMap (collectLocalBinders . snd) renamedPairs
                                   localSet' = foldl extendVarSet localSetWithBinders allInternalBinders
                                   visited'' = foldl extendVarSet visited' binders
                                   newExternals = filter (isResolvable localSet')
                                                        (nonDetEltsUniqSet allFVs)
                               in go fatCache (newExternals ++ rest) visited'' localSet' (fatBinds ++ acc) subAcc unres
                             Nothing ->
                               let uv = UnresolvedVar
                                     { uvKey    = varIdFn v
                                     , uvName   = occNameString (nameOccName (varName v))
                                     , uvModule = case nameModule_maybe (varName v) of
                                                    Just m  -> moduleNameString (moduleName m)
                                                    Nothing -> "<no module>"
                                     }
                               in go fatCache rest visited' localSet acc subAcc (uv : unres)

    -- | Modules whose bindings are over-collected into closures but never
    -- genuinely needed by JIT-compiled evals. GHC.Fingerprint (MD5) reaches
    -- rationalToDouble's transitive closure (via type fingerprinting) yet is
    -- only ever in dead branches for actual Double arithmetic; resolving it
    -- drags in the whole MD5/Int32 chain. Stopping resolution at the module
    -- boundary keeps that out of the table; the dead reference becomes a poison.
    isNeverResolve :: Var -> Bool
    isNeverResolve v = case nameModule_maybe (varName v) of
      Just m  -> let mn = moduleNameString (moduleName m)
                 in "Fingerprint" `isInfixOf` mn || "Typeable" `isInfixOf` mn
      Nothing -> False

    toRecPairs :: CoreBind -> [(Var, CoreExpr)]
    toRecPairs (NonRec b rhs) = [(b, rhs)]
    toRecPairs (Rec pairs)    = pairs

    isResolvable :: VarSet -> Var -> Bool
    isResolvable localSet v =
      (isGlobalId v || hasModule v)
      && not (elemVarSet v localSet)
      && not (isPrimOp v)
      && not (isDataCon v)
      && not (isMagicUnpackVar v)
      && not (isBottomingFunction v)

    -- | Check if a variable has a module association (i.e., belongs to some package
    -- even if not exported). Workers ($wf, $wlvl) generated by GHC are local
    -- (not isGlobalId) but still belong to their defining module. We need to resolve
    -- them via fat interface when their parent's unfolding is inlined.
    hasModule :: Var -> Bool
    hasModule v = case nameModule_maybe (varName v) of
      Just _  -> True
      Nothing -> False

    -- | Functions that always diverge (error, undefined, etc.).
    -- Don't resolve their unfoldings — doing so pulls in CallStack
    -- construction, SrcLoc formatting, exception machinery, and hundreds
    -- of unsupported primops. The translator emits them as Raise nodes
    -- that become runtime_error calls in the JIT.
    -- Note: we DON'T use isDeadEndId alone because GHC sometimes marks
    -- legitimate functions as bottoming (e.g., $wlvl from GHC.Internal.Unicode
    -- which is a partial case over Int#). Instead, we use name-based detection
    -- for known error functions, plus isDeadEndId only for GHC.Internal.* error
    -- modules.
    isBottomingFunction :: Var -> Bool
    isBottomingFunction v =
      nameIsBottoming || (isDeadEndId v && isErrorModule)
      where
        vname = occNameString (nameOccName (varName v))
        nameIsBottoming =
          vname `elem` [ "error", "errorWithoutStackTrace", "undefined"
                       , "divZeroError", "overflowError", "ratioZeroDenomError" ]
        isErrorModule = case nameModule_maybe (varName v) of
          Just m  -> let mn = moduleNameString (moduleName m)
                     in mn `elem` [ "GHC.Internal.Err", "GHC.Internal.Exception"
                                  , "GHC.Internal.IO.Exception"
                                  , "GHC.Internal.Control.Exception.Base" ]
          Nothing -> False

    bindersOfSet :: CoreBind -> VarSet
    bindersOfSet (NonRec b _) = unitVarSet b
    bindersOfSet (Rec pairs) = foldl (\s (b, _) -> extendVarSet s b) emptyVarSet pairs

    freeVarsOfBind :: CoreBind -> VarSet
    freeVarsOfBind (NonRec _ rhs) = exprSomeFreeVars (const True) rhs
    freeVarsOfBind (Rec pairs) = foldMap (exprSomeFreeVars (const True) . snd) pairs

    alphaRenameExpr :: VarSet -> CoreExpr -> CoreExpr
    alphaRenameExpr inScope expr =
      substExpr (mkEmptySubst (mkInScopeSet inScope)) expr

    collectLocalBinders :: CoreExpr -> [Var]
    collectLocalBinders = go'
      where
        go' (Lam b e)                 = b : go' e
        go' (Let (NonRec b rhs) body) = b : go' rhs ++ go' body
        go' (Let (Rec pairs) body)    = map fst pairs ++ concatMap (go' . snd) pairs ++ go' body
        go' (Case scrut b _ alts)     = b : go' scrut ++ concatMap goAlt alts
        go' (App f a)                 = go' f ++ go' a
        go' (Cast e _)                = go' e
        go' (Tick _ e)                = go' e
        go' _                         = []
        goAlt (Alt _ bs e)            = bs ++ go' e

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

    -- | Skip magic functions that Translate.hs handles specially.
    -- unpack* functions: their unfoldings use Addr# primops that we
    -- don't support; Translate desugars them to cons-cell chains.
    -- showDouble/$fShowDouble_$cshow: their unfoldings use Integer/GMP
    -- arithmetic; Translate emits a ShowDoubleAddr primop instead.
    isMagicUnpackVar :: Var -> Bool
    isMagicUnpackVar v =
      let name = occNameString (nameOccName (varName v))
      in name `elem` [ "unpackCString#", "unpackCStringUtf8#"
                      , "unpackAppendCString#"
                      , "unpackFoldrCString#", "unpackFoldrCStringUtf8#"
                      , "showDouble", "showDouble'"
                      , "$fShowDouble_$cshow" ]
         -- Block GHC's specialized showSignedFloat for Double and its
         -- arguments (pulls in floatToDigits → Integer/GMP pipeline).
         -- Translate.hs intercepts the call and emits ShowDoubleAddr.
         || "$fShowDouble_$s" `isPrefixOf` name
         || name == "$fShowDouble2"
         || name == "minExpt"

-- | Attempt to resolve a specialized Id by deriving its generic parent.
-- Parses the OccName for $s markers, strips them to get the generic name,
-- then looks up the generic Id in the same module via NameCache + EPS.
--
-- Returns @(genId, genUnfolding, aliasRhs)@ where:
--   * @genId@ + @genUnfolding@ register the generic parent (with its body),
--   * @aliasRhs@ is what the specialized binder should be bound to.
--
-- @aliasRhs@ is normally a bare @Var genId@ (arity-matched alias). But when
-- the generic has GREATER value arity than the specialization — i.e. a
-- dictionary value-arg was specialized away — a bare alias under-saturates
-- the generic (partial application → case trap). In that case we reconstruct
-- the concrete dictionary via 'reconstructSpecAlias' and bind to an
-- eta-expanded wrapper @\\\@tvs vs -> genId \@Tys dicts vs@.
attemptSpecFallback :: HscEnv -> Var -> IO (Maybe (Id, CoreExpr, CoreExpr))
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
                  case firstUnfolding genId of
                    Nothing   -> return Nothing
                    Just expr -> do
                      us <- mkSplitUniqSupply 'R'
                      let aliasRhs = case reconstructSpecAlias eps us genId specVar of
                                       Just wrapper -> wrapper
                                       Nothing      -> Var genId
                      return (Just (genId, expr, aliasRhs))
                _ -> return Nothing
  where
    firstUnfolding gid =
      case maybeUnfoldingTemplate (realIdUnfolding gid) of
        Just e  -> Just e
        Nothing -> maybeUnfoldingTemplate (idUnfolding gid)

-- | Reconstruct the alias RHS for a specialized binder whose generic parent
-- takes MORE value args (a dictionary was specialized away).
--
-- Builds @\\\@spTvs v1..vn -> genId \@Tys dict1..dictk v1..vn@ where:
--   * @Tys@ instantiate genId's type binders (discovered by unifying the
--     generic's result type with the specialization's via 'tcMatchTy'),
--   * @dictK@ are the concrete instance dictionaries for genId's class
--     constraints at those types, resolved via 'lookupInstEnv' on the EPS
--     global instance environment,
--   * @v1..vn@ are fresh binders for the specialization's value args.
--
-- Returns 'Nothing' (→ caller falls back to the bare alias, unchanged
-- behavior) when arity matches, the type match fails, or any constraint has
-- no unique instance — so arity-matched aliases and unresolvable cases are
-- never disturbed.
reconstructSpecAlias :: ExternalPackageState -> UniqSupply -> Id -> Var -> Maybe CoreExpr
reconstructSpecAlias eps us genId specVar = do
  let (genTvs, genTheta, genBody) = tcSplitSigmaTy (idType genId)
      (spTvs, spTheta, spBody)    = tcSplitSigmaTy (idType specVar)
      (genArgTys, _)              = tcSplitFunTys genBody
      (spArgTys, _)               = tcSplitFunTys spBody
      genValArity = length genTheta + length genArgTys
      spValArity  = length spTheta  + length spArgTys
  -- Only act when a dictionary value-arg was erased. Arity-matched aliases
  -- (the common path) return Nothing → caller keeps the bare alias.
  if genValArity <= spValArity
    then Nothing
    else do
      subst <- tcMatchTy genBody spBody
      let instEnvs = InstEnvs { ie_global  = eps_inst_env eps
                              , ie_local   = emptyInstEnv
                              , ie_visible = emptyModuleSet }
          resolveDict pty = do
            (cls, tys) <- getClassPredTys_maybe (substTy subst pty)
            case lookupInstEnv False instEnvs cls tys of
              ([(ci, dfunTyArgs)], _, _) ->
                Just (mkCoreApps (Var (instanceDFunId ci))
                                 (map Type (catMaybes dfunTyArgs)))
              _ -> Nothing   -- not a unique match → bail to bare alias
      dicts <- mapM resolveDict genTheta
      let typeArgs = map (substTy subst . mkTyVarTy) genTvs
          uniqs    = uniqsFromSupply us
          valBndrs = zipWith
                       (\u sty -> mkSysLocalOrCoVar (fsLit "wsp") u manyDataConTy (scaledThing sty))
                       uniqs spArgTys
          body = mkCoreApps (Var genId)
                   (map Type typeArgs ++ dicts ++ map Var valBndrs)
      Just (mkCoreLams (spTvs ++ valBndrs) body)

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
