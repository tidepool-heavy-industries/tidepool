{-# LANGUAGE TemplateHaskell #-}
{-# LANGUAGE ScopedTypeVariables #-}

module Tidepool.TypedSegment
  ( TypedSegmentPlan, typedSegmentPlan, typedSegmentPlanRoot
  , GeneratedSegmentOperations
  , typedSegmentPlanItems, typedSegmentReservationDigest, typedSegmentPlanDigest
  , TypedItemPlan(..), TypedItemBody(..)
  , TypedSegment, typedSegmentOriginalRoot, typedSegmentItems, typedSegmentRoots
  , PendingTypedSegment, pendingSegmentItems, pendingSegmentSupportRoots
  , TypedItem, typedItemPlan, typedItemRoot, typedItemInputs
  , typedItemActionType, typedItemCaptures, typedItemPredecessor, typedItemObservation
  , TypedItemInput, typedInputParameter, typedInputCapture
  , TypedCapture, CaptureOrigin(..), typedCaptureIdentifier, typedCaptureType
  , typedCaptureOrigin, typedCaptureFixity
  , TypedObservation, ObservationLift(..), typedObservationIdentifier
  , typedObservationLift, typedObservationValueType
  , TypedSegmentFailure(..), captureTypedSegment, closeTypedSegment, installTypedSegmentRoots
  ) where

import Tidepool.TypedSegment.Types
import Control.Exception (throwIO)
import Control.Monad (foldM, forM, forM_, unless, when)
import Control.Monad.IO.Class (liftIO)
import Data.Data (Data, Typeable, cast, gmapQ)
import Data.List (nub, partition)
import GHC (GhcTc, GhcRn, HsExpr(..), HsBind, HsBindLR(..), Pat(..), FixitySig(..), unLoc)
import GHC.Hs (HsLocalBinds, HsLocalBindsLR(..), LHsExpr, LPat, ABExport(..), AbsBinds(..), XXExprGhcTc(..), MatchGroup(..), Match(..), GRHSs(..), GRHS(..))
import GHC.Hs.Utils (collectHsBindBinders, collectHsBindsBinders, collectPatBinders, CollectFlag(..))
import GHC.Tc.Types.Evidence (HsWrapper(..), EvBind(..))
import GHC.HsToCore.Expr (dsLExpr, dsLocalBinds)
import GHC.HsToCore.Binds (dsTcEvBinds_s, dsEvBinds)
import GHC.HsToCore.Match (matchSinglePatVar)
import GHC.HsToCore.Monad (initDs, newSysLocalDs)
import GHC.HsToCore.Utils (cantFailMatchResult, extractMatchResult)
import GHC.Driver.Config.Core.Lint (initLintConfig)
import GHC.Core.Lint (lintExpr)
import GHC.Data.Bag (bagToList)
import GHC.Driver.Ppr (showSDoc)
import GHC.Core.Make (mkCoreLets, mkCoreTup, mkCoreConApps)
import GHC.Core.FVs (bindFreeVars, exprFreeVars, varTypeTyCoVars)
import GHC.Core.Subst (mkEmptySubst, extendSubst, substExpr)
import GHC.Types.Var.Env (mkInScopeSet, emptyVarEnv, extendVarEnv, lookupVarEnv)
import GHC.Types.Var.Set (unionVarSet, emptyVarSet, extendVarSet, elemVarSet)
import GHC.Core.Utils (exprType)
import GHC.Types.Name (mkExternalName, mkInternalName)
import GHC.Types.Name.Occurrence (mkVarOcc)
import GHC.Types.SrcLoc (noSrcSpan)
import GHC.Types.Unique.Supply (mkSplitUniqSupply, takeUniqFromSupply)
import GHC.Types.Unique.Set (elementOfUniqSet, nonDetEltsUniqSet)
import GHC.Types.Basic (Boxity(..))
import GHC.Builtin.Types (zonkAnyTyCon, unitTy, unitDataCon, manyDataConTy, intDataCon)
import GHC.Builtin.Types.Prim (intPrimTy)
import Data.ByteString qualified as BS
import Data.Map.Strict qualified as Map
import GHC.Core (Expr(..), Bind(..), Alt(..), AltCon(..), CoreExpr, CoreBind, bindersOf, mkApps, mkLams, collectArgs)
import GHC.Core.Type
  ( Type, splitTyConApp_maybe, splitFunTy_maybe, getTyVar_maybe, tyConsOfType, tyCoVarsOfType, mkVisFunTyMany, mkTyVarTy )
import GHC.Core.TyCo.Rep (Scaled(..))
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCon (TyCon, tyConName)
import GHC.Data.FastString (fsLit)
import GHC.Driver.Env (HscEnv, hsc_HPT, hsc_dflags)
import GHC.Driver.Env.Types (hsc_unit_env)
import GHC.Tc.Types (TcGblEnv, tcg_mod, tcg_type_env, tcg_binds, tcg_rn_decls, tcg_ev_binds)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Types.Avail (AvailInfo(..), availNames)
import GHC.Types.Id (Id, idName, idType, isLocalId, mkExportedVanillaId, mkLocalId, setIdType, setIdExported, setIdNotExported, isClassOpId_maybe)
import GHC.Core.Class (className)
import GHC.Core.InstEnv (instanceDFunId)
import GHC.Core.ConLike (ConLike(..))
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.PkgQual (PkgQual(OtherPkg))
import GHC.Types.TypeEnv (typeEnvIds)
import GHC.Types.Var (TyVar, isId, varUnique)
import GHC.Unit.Env (ue_units)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Unit.Home.ModInfo (lookupHpt, hm_iface, hm_details)
import GHC.Unit.Module (mkModuleName, moduleName)
import GHC.Unit.Module.ModDetails (md_types)
import GHC.Unit.Module.ModIface (mi_module, mi_exports)
import GHC.Unit.Module.ModGuts (ModGuts(..))
import Tidepool.Session (SessionModule(..), SessionModuleKind(..), Generation(..), sessionModuleString)
import GHC.Unit.Info (PackageName(..))
import GHC.Unit.State (lookupPackageName)
import Language.Haskell.TH.Syntax (addDependentFile, lift, loc_filename, location, runIO)
import System.FilePath (takeDirectory, (</>))
import System.IO (hPutStrLn, stderr)
import Tidepool.ExactScope (CanonicalInterfaceAdmission, resolveShippedHomeModule)
import Tidepool.Timing (readTimingEnabled)
import Tidepool.Json (jsonString)
import GHC.Utils.Outputable (ppr)

-- The source comparison and the actual exported Ids authenticate the operation
-- owner once. Extraction never resolves an operator from authored scope.
shippedResumeSource :: BS.ByteString
shippedResumeSource = BS.pack $(do
  here <- loc_filename <$> location
  let source = takeDirectory here </> ".." </> ".." </> "lib" </> "Tidepool" </> "Internal" </> "Resume.hs"
  addDependentFile source
  lift . BS.unpack =<< runIO (BS.readFile source))

data SegmentOperations = SegmentOperations
  { segmentPureId :: Id
  , segmentPureVariables :: [TyVar]
  , segmentPureValueVariable :: TyVar
  , segmentBindId :: Id
  , segmentBindVariables :: [TyVar]
  , segmentBindValueVariable :: TyVar
  , segmentBindResultVariable :: TyVar
  , segmentFailId :: Id
  , segmentFailVariables :: [TyVar]
  , segmentFailRowVariable :: TyVar
  , segmentEffConstructor :: TyCon
  , segmentSettleId :: Id
  , segmentSettleVariables :: [TyVar]
  , segmentSettleValueVariable :: TyVar
  , segmentResumeId :: Id
  }

resolveSegmentOperations :: HscEnv
  -> Map.Map (String,String) CanonicalInterfaceAdmission -> IO SegmentOperations
resolveSegmentOperations environment admitted = do
  owner <- resolveShippedHomeModule environment admitted "Tidepool.Internal.Resume" shippedResumeSource
    >>= maybe (throwIO UnprovedSegmentOperations) pure
  home <- maybe (throwIO UnprovedSegmentOperations) pure
    (lookupHpt (hsc_HPT environment) (moduleName owner))
  unless (mi_module (hm_iface home) == owner) (throwIO UnprovedSegmentOperations)
  let exports = concatMap availNames (mi_exports (hm_iface home))
      select name = unique UnprovedSegmentOperations UnprovedSegmentOperations
        [identifier | identifier <- typeEnvIds (md_types (hm_details home))
          , idName identifier `elem` exports, nameModule_maybe (idName identifier) == Just owner
          , occurrence identifier == name]
  pureId <- select "segmentPure"
  bindId <- select "segmentBind"
  failId <- select "segmentFail"
  settleId <- select "settle"
  resumeId <- select "resumeLifted"
  package <- maybe (throwIO UnprovedSegmentOperations) pure
    (lookupPackageName (ue_units (hsc_unit_env environment)) (PackageName (fsLit "freer-simple")))
  found <- findImportedModule environment (mkModuleName "Control.Monad.Freer.Internal") (OtherPkg package)
  effOwner <- case found of Found _ actual -> pure actual; _ -> throwIO UnprovedSegmentOperations
  let (pureVariables, purePredicates, pureBody) = tcSplitSigmaTy (idType pureId)
      (bindVariables, bindPredicates, bindBody) = tcSplitSigmaTy (idType bindId)
      (failVariables, failPredicates, failBody) = tcSplitSigmaTy (idType failId)
  case (splitFunTy_maybe pureBody, splitFunTy_maybe bindBody) of
    (Just (_, _, pureArgument, pureResult), Just (_, _, bindArgument, bindTail))
      | Just valueVariable <- getTyVar_maybe pureArgument
      , Just (constructor, [row, result]) <- splitTyConApp_maybe pureResult
      , Just rowVariable <- getTyVar_maybe row
      , eqType result pureArgument
      , nameModule_maybe (tyConName constructor) == Just effOwner
      , occNameString (nameOccName (tyConName constructor)) == "Eff"
      , Just (_, _, continuation, bindResult) <- splitFunTy_maybe bindTail
      , Just (_, _, input, output) <- splitFunTy_maybe continuation
      , Just inputVariable <- getTyVar_maybe input
      , Just (argumentConstructor, [argumentRow, argumentValue]) <- splitTyConApp_maybe bindArgument
      , Just (resultConstructor, [resultRow, resultValue]) <- splitTyConApp_maybe bindResult
      , Just resultVariable <- getTyVar_maybe resultValue
      , Just bindRowVariable <- getTyVar_maybe argumentRow
      , constructor == argumentConstructor, constructor == resultConstructor
      , eqType argumentRow resultRow, eqType argumentValue input, eqType output bindResult
      , null purePredicates, null bindPredicates
      , sameVariables pureVariables [valueVariable, rowVariable]
      , sameVariables bindVariables [inputVariable, resultVariable, bindRowVariable]
      , Just (_, _, _, failResult) <- splitFunTy_maybe failBody
      , Just (failConstructor, [failRow, failValue]) <- splitTyConApp_maybe failResult
      , failConstructor == constructor
      , Just failRowVariable <- getTyVar_maybe failRow
      , Just failValueVariable <- getTyVar_maybe failValue
      , sameVariables failVariables [failRowVariable, failValueVariable]
      , length failPredicates == 1 ->
          do
            let (settleVariables, settlePredicates, settleBody) = tcSplitSigmaTy (idType settleId)
                (_, resumePredicates, _) = tcSplitSigmaTy (idType resumeId)
            case splitFunTy_maybe settleBody of
              Just (_, _, settleInput, settleResult)
                | Just (inputConstructor, [settleRow, settleValue]) <- splitTyConApp_maybe settleInput
                , inputConstructor == constructor
                , Just settleRowVariable <- getTyVar_maybe settleRow
                , Just settleValueVariable <- getTyVar_maybe settleValue
                , Just (settled, [settledRow, settledValue]) <- splitTyConApp_maybe settleResult
                , nameModule_maybe (tyConName settled) == Just owner
                , occNameString (nameOccName (tyConName settled)) == "Settled"
                , eqType settleRow settledRow, eqType settleValue settledValue
                , null settlePredicates, null resumePredicates
                , sameVariables settleVariables [settleRowVariable, settleValueVariable] ->
                    pure (SegmentOperations pureId pureVariables valueVariable
                      bindId bindVariables inputVariable resultVariable
                      failId failVariables failRowVariable constructor
                      settleId settleVariables settleValueVariable resumeId)
              _ -> throwIO UnprovedSegmentOperations
    _ -> throwIO UnprovedSegmentOperations
  where
    sameVariables actual expected = length actual == length expected
      && all (`elem` actual) expected

pureCore :: SegmentOperations -> Type -> CoreExpr -> CoreExpr
pureCore operations row value = mkApps (Var (segmentPureId operations))
  (map (Type . argument) (segmentPureVariables operations) ++ [value])
  where
    argument variable
      | variable == segmentPureValueVariable operations = exprType value
      | otherwise = row

bindCore :: SegmentOperations -> Type -> Type -> Type -> CoreExpr -> CoreExpr -> CoreExpr
bindCore operations row input output action continuation = mkApps (Var (segmentBindId operations))
  (map (Type . argument) (segmentBindVariables operations) ++ [action, continuation])
  where
    argument variable
      | variable == segmentBindValueVariable operations = input
      | variable == segmentBindResultVariable operations = output
      | otherwise = row

settleCore :: SegmentOperations -> Type -> CoreExpr -> IO CoreExpr
settleCore operations row action = case splitTyConApp_maybe (exprType action) of
  Just (constructor, [actualRow, payload])
    | constructor == segmentEffConstructor operations, eqType actualRow row ->
        pure (mkApps (Var (segmentSettleId operations))
          (map (Type . argument payload) (segmentSettleVariables operations) ++ [action]))
  _ -> throwIO UnprovedSegmentOperations
  where
    argument payload variable
      | variable == segmentSettleValueVariable operations = payload
      | otherwise = row

-- GHC has checked the complete fixed-unit root once. All types below come
-- from that successful environment; only genuine local lets carry sigma types.
captureTypedSegment :: TypedSegmentPlan -> HscEnv
  -> Map.Map (String,String) CanonicalInterfaceAdmission -> TcGblEnv -> IO PendingTypedSegment
captureTypedSegment plan environment admitted checked = do
  operations <- resolveSegmentOperations environment admitted
  original <- unique MissingCaptureRoot AmbiguousCaptureRoot
    [identifier | identifier <- typeEnvIds (tcg_type_env checked)
      , occurrence identifier == typedSegmentPlanRoot plan
      , nameModule_maybe (idName identifier) == Just (tcg_mod checked)]
  checker <- if any isObservation (typedSegmentPlanItems plan)
    then Just <$> unique UnprovedSegmentOperations UnprovedSegmentOperations
      [identifier | identifier <- typeEnvIds (tcg_type_env checked)
        , occurrence identifier == "__tidepoolCellExpression"
        , nameModule_maybe (idName identifier) == Just (tcg_mod checked)
        , Just owner <- [isClassOpId_maybe identifier]
        , nameModule_maybe (className owner) == Just (tcg_mod checked)
        , occNameString (nameOccName (className owner)) == "TidepoolCellExpression"]
    else pure Nothing
  let (variables, predicates, bodyType) = tcSplitSigmaTy (idType original)
  row <- case splitTyConApp_maybe bodyType of
    Just (constructor, [row, payload])
      | constructor == segmentEffConstructor operations, eqType payload unitTy
      , null variables, null predicates -> pure row
    _ -> throwIO UnprovedFixedUnitRoot
  checkClosedType (-1) row
  abstraction <- unique UnprovedRootAbstraction UnprovedRootAbstraction
    [bindings | binding <- tcg_binds checked, XHsBindsLR bindings <- [unLoc binding]
      , [exported] <- [abs_exports bindings], abe_poly exported == original
      , WpHole <- [abe_wrap exported]]
  unless (null (abs_tvs abstraction) && null (abs_ev_vars abstraction)) $
    throwIO UnprovedRootAbstraction
  body <- rootBody abstraction
  retained <- retainChain operations checker Nothing (typedSegmentPlanItems plan) (unLoc body)
  renamed <- maybe (throwIO MissingRenamedCaptures) pure (tcg_rn_decls checked)
  exports <- identityExports checked
  timing <- readTimingEnabled
  let fixities = Map.fromList
        [(unLoc name, fixity) | FixitySig _ names fixity <- (collect renamed :: [FixitySig GhcRn])
          , name <- names]
      tops = collectHsBindsBinders CollNoDictBinders (tcg_binds checked)
      known = tops ++ typeEnvIds (tcg_type_env checked) ++ map eb_lhs (bagToList (tcg_ev_binds checked))
  items <- forM (zip retained (scanl (++) [] (map retainedCaptures retained))) $ \(item, previous) -> do
    let itemPlan = retainedPlan item
        ordinal = plannedItemOrdinal itemPlan
    (action, captures, observation) <- lowerItem operations row item environment checked
    let free = nonDetEltsUniqSet (exprFreeVars action)
        inputs = [identifier | identifier <- previous, identifier `elem` free]
    settled <- settleCore operations row action
    (_, lowered) <- initDs environment checked $
      dsEvBinds (tcg_ev_binds checked) $ \topEvidence ->
        dsTcEvBinds_s (abs_ev_binds abstraction) $ \evidence ->
          pure (keepRequiredEvidence (topEvidence ++ evidence) (mkLams inputs settled))
    rawCore <- maybe (throwIO (ItemDesugaringFailed ordinal)) pure lowered
    let core = applyIdentityExports exports rawCore
    when timing $ forM_ [identifier | identifier <- nonDetEltsUniqSet (exprFreeVars rawCore)
        , isId identifier, isLocalId identifier] $ \identifier -> do
      let actual = case lookup identifier exports of Just exported -> exported; Nothing -> identifier
      hPutStrLn stderr $ "tidepool-typed-module-reference ordinal=" ++ show ordinal
        ++ " name=" ++ jsonString (showSDoc (hsc_dflags environment) (ppr (idName identifier)))
        ++ " unique=" ++ show (varUnique identifier)
        ++ " identity_export=" ++ show (identifier /= actual)
        ++ " exported_unique=" ++ show (varUnique actual)
        ++ " module_top=" ++ show (actual `elem` tops)
        ++ " type_env=" ++ show (identifier `elem` typeEnvIds (tcg_type_env checked))
    mapM_ (checkClosedType ordinal . idType) inputs
    checkClosedType ordinal (exprType core)
    let payloadType = exprType (mkCoreTup (map Var captures))
    case splitTyConApp_maybe (exprType action) of
      Just (constructor, [actionRow, payload])
        | constructor == segmentEffConstructor operations, eqType row actionRow
        , eqType payload payloadType -> pure ()
      _ -> throwIO (UnprovedItemPayload ordinal)
    descriptors <- forM captures $ \identifier -> do
      checkClosedType ordinal (idType identifier)
      let origin = case plannedItemBody itemPlan of
            LetItem{} -> AuthoredLet
            ActionItem{} -> ActionOccurrence
            ObservationItem{} -> BareObservation
      pure (TypedCapture identifier origin (Map.lookup (idName identifier) fixities))
    checkItemCore environment ordinal known core
    when (any ((== plannedItemEntry itemPlan) . occurrence) known) $
      throwIO (ExistingItemRoot ordinal)
    supply <- mkSplitUniqSupply 'i'
    let (uniqueId, _) = takeUniqFromSupply supply
        root = mkExportedVanillaId (mkExternalName uniqueId (tcg_mod checked)
          (mkVarOcc (plannedItemEntry itemPlan)) noSrcSpan) (exprType core)
    pure (TypedItem itemPlan root [TypedItemInput identifier identifier | identifier <- inputs]
      (exprType action) descriptors (retainedPredecessor item) observation core)
  auxiliary <- auxiliaryRoots operations environment checked
  pure (PendingTypedSegment (TypedSegment original items auxiliary) tops)
  where
    isObservation item = case plannedItemBody item of ObservationItem{} -> True; _ -> False

-- GHC's complete-signature AbsBinds can contain a fresh monomorphic clone
-- even when the export is an identity. Its actual export pair, not its Name,
-- proves this substitution. General type/evidence wrappers are not inverted.
identityExports :: TcGblEnv -> IO [(Id,Id)]
identityExports checked = do
  let pairs = [(abe_mono exported, abe_poly exported)
        | binding <- tcg_binds checked
        , XHsBindsLR abstraction <- [unLoc binding]
        , null (abs_tvs abstraction), null (abs_ev_vars abstraction)
        , exported <- abs_exports abstraction
        , WpHole <- [abe_wrap exported]
        , eqType (idType (abe_mono exported)) (idType (abe_poly exported))
        , nameModule_maybe (idName (abe_poly exported)) == Just (tcg_mod checked)]
  unless (length (nub (map fst pairs)) == length pairs) $
    throwIO UnprovedRootAbstraction
  pure pairs

applyIdentityExports :: [(Id,Id)] -> CoreExpr -> CoreExpr
applyIdentityExports exports core = substExpr substitution core
  where
    used = [(local, global) | (local, global) <- exports
      , local `elementOfUniqSet` exprFreeVars core]
    scope = mkInScopeSet (foldl' unionVarSet (exprFreeVars core)
      [exprFreeVars (Var global) | (_,global) <- used])
    substitution = foldl' (\current (local,global) -> extendSubst current local (Var global))
      (mkEmptySubst scope) used

-- Only GHC evidence groups enter this closure; authored local bindings remain
-- in the item body. Keep every demanded dictionary/coercion dependency and
-- complete recursive group, in its original lexical order.
keepRequiredEvidence :: [CoreBind] -> CoreExpr -> CoreExpr
keepRequiredEvidence bindings body = mkCoreLets (requiredBindings bindings body) body

requiredBindings :: [CoreBind] -> CoreExpr -> [CoreBind]
requiredBindings bindings body = filter (demanded required) bindings
  where
    groups = [(binding, foldl' unionVarSet (bindFreeVars binding)
      (map varTypeTyCoVars (bindersOf binding))) | binding <- bindings]
    required = close (exprFreeVars body) groups
    demanded names binding = any (`elementOfUniqSet` names) (bindersOf binding)
    close names remaining = case partition (demanded names . fst) remaining of
      ([], _) -> names
      (selected, pending) -> close
        (foldl' unionVarSet names (map snd selected)) pending

-- These generic entries preserve the existing resume/apply runtime ABI. Their
-- telescope is the actual shipped operation telescope, independent of any item.
auxiliaryRoots :: SegmentOperations -> HscEnv -> TcGblEnv -> IO [CoreBind]
auxiliaryRoots operations environment checked = do
  let (variables, predicates, body) = tcSplitSigmaTy (idType (segmentResumeId operations))
  unless (null predicates) (throwIO UnprovedSegmentOperations)
  case splitFunTy_maybe body of
    Just (_, _, queueType, tailType)
      | Just (_, _, valueType, actionType) <- splitFunTy_maybe tailType
      , Just (constructor, [row, _payload]) <- splitTyConApp_maybe actionType
      , constructor == segmentEffConstructor operations -> do
          (_, built) <- initDs environment checked $ do
            queue <- newSysLocalDs (Scaled manyDataConTy queueType)
            value <- newSysLocalDs (Scaled manyDataConTy valueType)
            function <- newSysLocalDs (Scaled manyDataConTy
              (mkVisFunTyMany valueType actionType))
            integer <- newSysLocalDs (Scaled manyDataConTy intPrimTy)
            intFunction <- newSysLocalDs (Scaled manyDataConTy
              (mkVisFunTyMany (exprType (mkCoreConApps intDataCon [Var integer])) actionType))
            resumed <- liftIO (settleCore operations row
              (mkApps (Var (segmentResumeId operations)) (map (Type . mkTyVarTy) variables ++ [Var queue, Var value])))
            applied <- liftIO (settleCore operations row (App (Var function) (Var value)))
            intApplied <- liftIO (settleCore operations row
              (App (Var intFunction) (mkCoreConApps intDataCon [Var integer])))
            -- Unused telescope variables remain valid erased arguments. No
            -- action result variable becomes a captured value quantifier.
            pure [("__resume", mkLams (variables ++ [queue,value]) resumed),
                  ("__applyValue", mkLams (variables ++ [function,value]) applied),
                  ("__applyEntry", mkLams (variables ++ [intFunction,integer]) intApplied)]
          expressions <- maybe (throwIO UnprovedSegmentOperations) pure built
          forM expressions $ \(name, core) -> do
            when (any ((== name) . occurrence) (typeEnvIds (tcg_type_env checked))) $
              throwIO UnprovedSegmentOperations
            checkItemCore environment (-1) [] core
            supply <- mkSplitUniqSupply 'r'
            let (uniqueId, _) = takeUniqFromSupply supply
                root = mkExportedVanillaId (mkExternalName uniqueId (tcg_mod checked)
                  (mkVarOcc name) noSrcSpan) (exprType core)
            pure (NonRec root core)
    _ -> throwIO UnprovedSegmentOperations

-- The decoded HPT is the authority for each supplied global. Captures remain
-- values; closing an item's prior-value parameters never replays its predecessor.
closeTypedSegment :: HscEnv -> [(Id,Id)] -> PendingTypedSegment -> IO PendingTypedSegment
closeTypedSegment environment globals pending = do
  let segment = pendingSegmentValue pending
  let expected = [(typedCaptureIdentifier capture, plannedItemGeneration (typedItemPlan item))
        | item <- typedSegmentItems segment, capture <- typedItemCaptures item]
  unless (length globals == length expected && length (nub (map fst globals)) == length globals) $
    throwIO UnprovedSegmentGlobals
  forM_ expected $ \(original, generation) -> do
    global <- unique UnprovedSegmentGlobals UnprovedSegmentGlobals
      [target | (source, target) <- globals, source == original]
    let moduleName' = mkModuleName (sessionModuleString (SessionModule ValMod (Generation generation)))
    home <- maybe (throwIO UnprovedSegmentGlobals) pure (lookupHpt (hsc_HPT environment) moduleName')
    unless (nameModule_maybe (idName global) == Just (mi_module (hm_iface home))
      && occurrence global == occurrence original && eqType (idType original) (idType global)
      && any (\actual -> actual == global && eqType (idType actual) (idType global))
        (typeEnvIds (md_types (hm_details home)))) (throwIO UnprovedSegmentGlobals)
  items <- forM (typedSegmentItems segment) $ \item -> do
    arguments <- forM (typedItemInputs item) $ \input -> do
      target <- unique UnprovedSegmentGlobals UnprovedSegmentGlobals
        [global | (source, global) <- globals, source == typedInputCapture input]
      pure (typedInputParameter input, Var target)
    core <- applyInputs (plannedItemOrdinal (typedItemPlan item)) (typedItemCore item) arguments
    checkItemCore environment (plannedItemOrdinal (typedItemPlan item))
      (pendingSegmentTopIdentifiers pending) core
    pure item { typedItemRoot = setIdType (typedItemRoot item) (exprType core), typedItemCore = core }
  pure pending { pendingSegmentValue = segment { typedSegmentItems = items } }
  where
    applyInputs _ expression [] = pure expression
    applyInputs ordinal (Let bindings body) arguments = Let bindings <$> applyInputs ordinal body arguments
    applyInputs ordinal (Tick tick body) arguments = Tick tick <$> applyInputs ordinal body arguments
    applyInputs ordinal (Lam binder body) ((parameter, argument) : rest)
      | binder == parameter, eqType (idType binder) (exprType argument) = do
          let scope = mkInScopeSet (exprFreeVars body `unionVarSet` exprFreeVars argument)
              substitution = extendSubst (mkEmptySubst scope) binder argument
          applyInputs ordinal (substExpr substitution body) rest
    applyInputs ordinal _ _ = throwIO (UnprovedItemParameters ordinal)

-- Only the actual item roots survive into the executable target interface.
-- The private checker classes served inference and confer no published type.
installTypedSegmentRoots :: HscEnv -> PendingTypedSegment -> ModGuts -> IO (TypedSegment,ModGuts)
installTypedSegmentRoots environment pending guts = do
  let segment = pendingSegmentValue pending
  timing <- readTimingEnabled
  unless (nameModule_maybe (idName (typedSegmentOriginalRoot segment)) == Just (mg_module guts)) $
    throwIO UnprovedRootAbstraction
  let synthetic constructor = nameModule_maybe (tyConName constructor) == Just (mg_module guts)
        && occNameString (nameOccName (tyConName constructor)) `elem` ["TidepoolCellExpression", "TidepoolCellPure"]
      privateTypes = filter synthetic (mg_tcs guts)
      mentionsPrivate ty = any (`elementOfUniqSet` tyConsOfType ty) privateTypes
      roots = typedSegmentRoots segment
      metadataIds = map instanceDFunId (mg_insts guts)
      metadataRoots = foldl' extendVarSet emptyVarSet metadataIds
      -- Tidy resolves every retained instance through an external final Id.
      -- These actual metadata roots are retained without adding source exports.
      retain identifier
        | identifier `elemVarSet` metadataRoots = setIdExported identifier
        | otherwise = setIdNotExported identifier
      unexport (NonRec identifier rhs) = NonRec (retain identifier) rhs
      unexport (Rec members) = Rec [(retain identifier,rhs) | (identifier,rhs) <- members]
      bindings = map unexport (mg_binds guts) ++ roots
      identifiers = concatMap bindersOf bindings
  forM_ (typedSegmentItems segment) $ \item ->
    when (mentionsPrivate (idType (typedItemRoot item))
      || any (mentionsPrivate . typedCaptureType) (typedItemCaptures item)) $
        throwIO (UnprovedItemPayload (plannedItemOrdinal (typedItemPlan item)))
  graph <- foldM addDefinition emptyVarEnv
    [(identifier,index,binding) | (index,binding) <- zip [0 :: Int ..] bindings
      , identifier <- bindersOf binding]
  forM_ metadataIds $ \identifier -> case lookupVarEnv graph identifier of
    Just (actual,_,_) | eqType (idType actual) (idType identifier) -> pure ()
    _ -> throwIO (OpenItemCore (-1) [occurrence identifier])
  let reverseReferences = foldl' addReverse emptyVarEnv
        [(identifier,rhs) | binding <- bindings, (identifier,rhs) <- bindingPairs binding]
      originalDependents = findDependents reverseReferences emptyVarSet
        [typedSegmentOriginalRoot segment]
      moduleTops = foldl' extendVarSet emptyVarSet (pendingSegmentTopIdentifiers pending)
      checkReferences ordinal core = forM_ (nonDetEltsUniqSet (exprFreeVars core)) $ \reference ->
        case lookupVarEnv graph reference of
          Nothing -> when (reference `elemVarSet` moduleTops) $
            throwIO (OpenItemCore ordinal [occurrence reference])
          Just (identifier,_,_) -> unless (eqType (idType identifier) (idType reference)) $
            throwIO (OpenItemCore ordinal [occurrence reference])
      demandedGroups selected [] = selected
      demandedGroups selected (reference : rest) = case lookupVarEnv graph reference of
        Nothing -> demandedGroups selected rest
        Just (_,index,_) | Map.member index selected -> demandedGroups selected rest
        Just (_,index,binding) -> demandedGroups (Map.insert index binding selected)
          (nonDetEltsUniqSet (foldl' unionVarSet (bindFreeVars binding)
            (map varTypeTyCoVars (bindersOf binding))) ++ rest)
  -- Keep the whole compiler graph, including support SCCs. Only issued roots
  -- and metadata-mandated dfuns remain externally retained. Ordinary GHC DCE
  -- owns unused inference scaffolding; mg_exports names only the issued roots.
  support <- foldM (\selected binding -> do
    let ordinal = case [plannedItemOrdinal (typedItemPlan item)
          | item <- typedSegmentItems segment, typedItemRoot item `elem` bindersOf binding] of
          [itemOrdinal] -> itemOrdinal
          _ -> -1
    checkBindingType ordinal binding
    foldM (\current core -> do
      checkReferences ordinal core
      checkItemCore environment ordinal identifiers core
      when (any (`elemVarSet` originalDependents) (nonDetEltsUniqSet (exprFreeVars core))) $
        throwIO (OpenItemCore ordinal [occurrence (typedSegmentOriginalRoot segment)])
      when timing $ forM_ [reference | reference <- nonDetEltsUniqSet (exprFreeVars core)
          , reference `elemVarSet` moduleTops] $ \reference ->
        hPutStrLn stderr $ "tidepool-typed-module-closure ordinal=" ++ show ordinal
          ++ " name=" ++ jsonString (showSDoc (hsc_dflags environment) (ppr (idName reference)))
          ++ " unique=" ++ show (varUnique reference) ++ " bound=True type_eq=True"
      pure (demandedGroups current (nonDetEltsUniqSet (exprFreeVars core))))
      selected (bindingExpressions binding)) Map.empty roots
  -- Shared demanded groups are checked once, with their complete Rec scope.
  forM_ (Map.elems support) $ \binding -> do
    checkBindingType (-1) binding
    forM_ (bindingExpressions binding) $ \rhs -> do
      checkReferences (-1) rhs
      checkItemCore environment (-1) identifiers rhs
  pure (segment, guts { mg_binds = bindings
            , mg_exports = [Avail (idName identifier) | NonRec identifier _ <- roots] })
  where
    addDefinition graph (identifier,index,binding) = case lookupVarEnv graph identifier of
      Just _ -> throwIO (OpenItemCore (-1) [occurrence identifier])
      Nothing -> pure (extendVarEnv graph identifier (identifier,index,binding))
    addReverse graph (identifier,rhs) = foldl' (\current reference ->
      extendVarEnv current reference (identifier : maybe [] id (lookupVarEnv current reference)))
      graph (nonDetEltsUniqSet (exprFreeVars rhs))
    findDependents _ found [] = found
    findDependents reverseReferences found (identifier : rest)
      | identifier `elemVarSet` found = findDependents reverseReferences found rest
      | otherwise = findDependents reverseReferences (extendVarSet found identifier)
          (maybe [] id (lookupVarEnv reverseReferences identifier) ++ rest)
    checkBindingType ordinal binding = forM_ (bindingPairs binding) $ \(identifier,rhs) ->
      unless (eqType (idType identifier) (exprType rhs)) $
        throwIO (OpenItemCore ordinal [occurrence identifier])
    bindingPairs (NonRec identifier rhs) = [(identifier,rhs)]
    bindingPairs (Rec values) = values
    bindingExpressions (NonRec _ rhs) = [rhs]
    bindingExpressions (Rec values) = map snd values

checkClosedType :: Int -> Type -> IO ()
checkClosedType ordinal ty = do
  when (zonkAnyTyCon `elementOfUniqSet` tyConsOfType ty) $
    throwIO (UnresolvedItemType ordinal)
  let free = nonDetEltsUniqSet (tyCoVarsOfType ty)
  unless (null free) (throwIO (OpenCaptureType ordinal (map occurrence free)))

checkItemCore :: HscEnv -> Int -> [Id] -> CoreExpr -> IO ()
checkItemCore environment ordinal known core = do
  let remaining = [variable | variable <- nonDetEltsUniqSet (exprFreeVars core)
        , not (isId variable) || (isLocalId variable && variable `notElem` known)]
  unless (null remaining) (throwIO (OpenItemCore ordinal (map occurrence remaining)))
  case lintExpr (initLintConfig (hsc_dflags environment) known) core of
    Nothing -> pure ()
    Just errors -> throwIO (CaptureCoreLintFailed
      (map (showSDoc (hsc_dflags environment)) (bagToList errors)))

data RetainedItem = RetainedItem
  { retainedPlan :: TypedItemPlan
  , retainedCaptures :: [Id]
  , retainedPredecessor :: Maybe Id
  , retainedBody :: RetainedBody
  }

data RetainedBody
  = RetainedAction Id (LPat GhcTc) (LHsExpr GhcTc) (LHsExpr GhcTc) (Match GhcTc (LHsExpr GhcTc))
  | RetainedLet (HsLocalBinds GhcTc)
  | RetainedObservation Id (LHsExpr GhcTc)

-- The only recursive visit follows the enclosing root's successful chain.
-- Nested authored RHS computations never supply segment item witnesses.
retainChain :: SegmentOperations -> Maybe Id -> Maybe Id -> [TypedItemPlan] -> HsExpr GhcTc -> IO [RetainedItem]
retainChain operations _ _ [] expression = do
  unless (neutralTerminal operations expression) (throwIO UnprovedRootContinuation)
  pure []
retainChain operations checker predecessor (item : items) expression = do
  let ordinal = plannedItemOrdinal item
  case plannedItemBody item of
    LetItem markerName names
      | HsLet _ locals continuation <- stripTyped expression -> do
          let identifiers = exportedIds locals
          marker <- unique (UnprovedItemSequence ordinal) (UnprovedItemSequence ordinal)
            [identifier | identifier <- identifiers, occurrence identifier == markerName]
          unless (eqType (idType marker) unitTy) (throwIO (UnprovedItemSequence ordinal))
          captures <- forM names $ \name -> unique (MissingLetBinder ordinal name) (AmbiguousLetBinder ordinal name)
            [identifier | identifier <- identifiers, occurrence identifier == name]
          let authored = filter (/= marker) identifiers
          unless (length authored == length captures && all (`elem` captures) authored) $
            throwIO (IncompleteLetCapture ordinal)
          rest <- retainChain operations checker (Just marker) items (unLoc continuation)
          pure (RetainedItem item captures predecessor (RetainedLet locals) : rest)
    ActionItem stepName probeName markerName names -> do
      (rhs, continuation) <- bindApplication operations expression
      (patterns, body) <- lambdaRhs continuation
      marker <- unique (UnprovedItemSequence ordinal) (UnprovedItemSequence ordinal)
        [identifier | pattern <- patterns, Just identifier <- [lazyTypedVariable pattern]
          , occurrence identifier == markerName]
      unless (length patterns == 1) (throwIO (UnprovedItemSequence ordinal))
      argument <- identityOccurrence ordinal stepName probeName rhs
      (pattern, success, failure, match) <- patternContinuation ordinal marker body
      let captures = collectPatBinders CollNoDictBinders pattern
      unless (map occurrence captures == names) (throwIO (UnprovedItemSequence ordinal))
      rest <- retainChain operations checker (Just marker) items (unLoc success)
      pure (RetainedItem item captures predecessor
        (RetainedAction marker pattern argument failure match) : rest)
    ObservationItem probeName observationName -> do
      (rhs, continuation) <- bindApplication operations expression
      (patterns, body) <- lambdaRhs continuation
      unitMarker <- unique (UnprovedItemSequence ordinal) (UnprovedItemSequence ordinal)
        [identifier | pattern <- patterns, Just identifier <- [lazyTypedVariable pattern]
          , occurrence identifier == observationName ++ "_unit"]
      unless (length patterns == 1 && eqType (idType unitMarker) unitTy) $
        throwIO (UnprovedItemSequence ordinal)
      (function, argument) <- application rhs
      (probePatterns, probeBody) <- lambdaRhs function
      probe <- unique (UnprovedExpressionOccurrence ordinal) (UnprovedExpressionOccurrence ordinal)
        [identifier | pattern <- probePatterns, Just identifier <- [lazyTypedVariable pattern]
          , occurrence identifier == probeName]
      unless (length probePatterns == 1 && maybe False (\method -> checkerProbe method probe probeBody) checker) $
        throwIO (UnprovedExpressionOccurrence ordinal)
      rest <- retainChain operations checker (Just unitMarker) items (unLoc body)
      pure (RetainedItem item [] predecessor (RetainedObservation probe argument) : rest)
    _ -> throwIO (UnprovedItemSequence ordinal)

bindApplication :: SegmentOperations -> HsExpr GhcTc -> IO (LHsExpr GhcTc, LHsExpr GhcTc)
bindApplication operations expression = case stripTyped expression of
  HsApp _ first continuation
    | HsApp _ function argument <- stripTyped (unLoc first)
    , HsVar _ identifier <- stripTyped (unLoc function)
    , unLoc identifier == segmentBindId operations -> pure (argument, continuation)
  _ -> throwIO UnprovedRootContinuation

patternContinuation :: Int -> Id -> LHsExpr GhcTc
  -> IO (LPat GhcTc, LHsExpr GhcTc, LHsExpr GhcTc, Match GhcTc (LHsExpr GhcTc))
patternContinuation ordinal marker body = case stripTyped (unLoc body) of
  HsCase _ scrutinee MG { mg_alts = alternatives }
    | HsVar _ identifier <- stripTyped (unLoc scrutinee), unLoc identifier == marker
    , [successful, failing] <- unLoc alternatives -> do
        (patterns, success) <- matchRhs (unLoc successful)
        (failurePatterns, failure) <- matchRhs (unLoc failing)
        pattern <- unique (UnprovedItemSequence ordinal) (UnprovedItemSequence ordinal) patterns
        unless (isWildcard failurePatterns) $
          throwIO (UnprovedItemSequence ordinal)
        pure (pattern, success, failure, unLoc successful)
  _ -> throwIO (UnprovedItemSequence ordinal)
  where
    isWildcard [wildcard] = case unLoc wildcard of WildPat{} -> True; _ -> False
    isWildcard _ = False

identityOccurrence :: Int -> String -> String -> LHsExpr GhcTc -> IO (LHsExpr GhcTc)
identityOccurrence ordinal stepName probeName expression = do
  (stepFunction, nested) <- application expression
  proveIdentity stepName stepFunction
  (probeFunction, argument) <- application nested
  proveIdentity probeName probeFunction
  pure argument
  where
    proveIdentity name function = do
      (patterns, body) <- lambdaRhs function
      case patterns of
        [pattern] | Just identifier <- lazyTypedVariable pattern
          , occurrence identifier == name
          , HsVar _ returned <- stripTyped (unLoc body), unLoc returned == identifier -> pure ()
        _ -> throwIO (UnprovedOccurrenceProbe ordinal)

checkerProbe :: Id -> Id -> LHsExpr GhcTc -> Bool
checkerProbe method probe body = case stripTyped (unLoc body) of
  HsApp _ function argument
    | HsVar _ identifier <- stripTyped (unLoc function)
    , unLoc identifier == method
    , HsVar _ value <- stripTyped (unLoc argument), unLoc value == probe -> True
  _ -> False

lowerItem :: SegmentOperations -> Type -> RetainedItem -> HscEnv -> TcGblEnv
  -> IO (CoreExpr, [Id], Maybe TypedObservation)
lowerItem operations row item environment checked = do
  let ordinal = plannedItemOrdinal (retainedPlan item)
      captures = retainedCaptures item
  (_, lowered) <- initDs environment checked $ case retainedBody item of
    RetainedLet locals -> do
      action <- dsLocalBinds locals (pureCore operations row (mkCoreTup (map Var captures)))
      pure (action, captures, Nothing)
    RetainedAction marker pattern argument failure successfulMatch -> do
      action <- dsLExpr argument
      let payload = mkCoreTup (map Var captures)
          success = pureCore operations row payload
      failed <- dsLExpr failure >>= liftIO . replaceFailurePayload operations row (exprType payload) ordinal
      result <- matchSinglePatVar marker Nothing (m_ctxt successfulMatch) pattern
        (exprType success) (cantFailMatchResult success)
      matched <- extractMatchResult result failed
      pure (bindCore operations row (idType marker) (exprType payload) action (Lam marker matched), captures, Nothing)
    RetainedObservation probe argument -> do
      value <- dsLExpr argument
      let occurrenceType = idType probe
      liftIO (checkClosedType ordinal occurrenceType)
      (liftKind, resultType) <- case splitTyConApp_maybe occurrenceType of
        Just (constructor, [actualRow, payload]) | constructor == segmentEffConstructor operations -> do
          unless (eqType actualRow row) (liftIO (throwIO (WrongObservationEffectRow ordinal)))
          pure (EffectfulObservation, payload)
        _ -> pure (PureObservation, occurrenceType)
      liftIO (checkClosedType ordinal resultType)
      unitParameter <- newSysLocalDs (Scaled manyDataConTy unitTy)
      caseBinder <- newSysLocalDs (Scaled manyDataConTy unitTy)
      observed <- newSysLocalDs (Scaled manyDataConTy resultType)
      let thunk expression = Lam unitParameter (Case (Var unitParameter) caseBinder resultType
            [Alt (DataAlt unitDataCon) [] expression])
      capture <- liftIO $ freshCapture checked ordinal (plannedItemBody (retainedPlan item))
        (exprType (thunk (Var observed)))
      let keep expression = Let (NonRec capture (thunk expression))
            (pureCore operations row (Var capture))
          retained = case liftKind of
            PureObservation -> keep value
            EffectfulObservation -> bindCore operations row resultType (exprType (thunk (Var observed)))
              value (Lam observed (keep (Var observed)))
      pure (retained, [capture], Just (TypedObservation probe liftKind resultType))
  maybe (throwIO (ItemDesugaringFailed ordinal)) pure lowered

freshCapture :: TcGblEnv -> Int -> TypedItemBody -> Type -> IO Id
freshCapture _checked ordinal body ty = case body of
  ObservationItem _ name -> do
    supply <- mkSplitUniqSupply 'o'
    let (uniqueId, _) = takeUniqFromSupply supply
    pure (mkLocalId (mkInternalName uniqueId (mkVarOcc name) noSrcSpan) manyDataConTy ty)
  _ -> throwIO (UnprovedCaptureBinding ordinal)

replaceFailurePayload :: SegmentOperations -> Type -> Type -> Int -> CoreExpr -> IO CoreExpr
replaceFailurePayload operations row payload ordinal expression = case collectArgs expression of
  (Var identifier, arguments)
    | identifier == segmentFailId operations
    , length arguments == length (segmentFailVariables operations) + 2
    , (types, [dictionary, message]) <- splitAt (length (segmentFailVariables operations)) arguments
    , and (zipWith actualType (segmentFailVariables operations) types) ->
        pure (mkApps (Var identifier) (map (Type . replacement) (segmentFailVariables operations) ++ [dictionary, message]))
  _ -> throwIO (UnprovedItemSequence ordinal)
  where
    actualType variable (Type ty)
      | variable == segmentFailRowVariable operations = eqType ty row
      | otherwise = eqType ty unitTy
    actualType _ _ = False
    replacement variable
      | variable == segmentFailRowVariable operations = row
      | otherwise = payload

rootBody :: AbsBinds -> IO (LHsExpr GhcTc)
rootBody abstraction = do
  exported <- unique UnprovedRootAbstraction UnprovedRootAbstraction (abs_exports abstraction)
  matches <- unique UnprovedRootAbstraction UnprovedRootAbstraction
    [matches | binding <- abs_binds abstraction
      , FunBind { fun_id = identifier, fun_ext = (WpHole, []), fun_matches = matches } <- [unLoc binding]
      , unLoc identifier == abe_mono exported]
  (patterns, body) <- singleRhs matches
  unless (null patterns) (throwIO UnprovedRootContinuation)
  pure body

singleRhs :: MatchGroup GhcTc (LHsExpr GhcTc) -> IO ([LPat GhcTc], LHsExpr GhcTc)
singleRhs MG { mg_alts = alternatives } = do
  match <- unique UnprovedRootContinuation UnprovedRootContinuation (map unLoc (unLoc alternatives))
  matchRhs match

matchRhs :: Match GhcTc (LHsExpr GhcTc) -> IO ([LPat GhcTc], LHsExpr GhcTc)
matchRhs Match { m_pats = patterns, m_grhss = GRHSs _ [grhs] (EmptyLocalBinds _) }
  | GRHS _ [] body <- unLoc grhs = pure (unLoc patterns, body)
matchRhs _ = throwIO UnprovedRootContinuation

lambdaRhs :: LHsExpr GhcTc -> IO ([LPat GhcTc], LHsExpr GhcTc)
lambdaRhs expression = case stripTyped (unLoc expression) of
  HsLam _ _ matches -> singleRhs matches
  _ -> throwIO UnprovedRootContinuation

application :: LHsExpr GhcTc -> IO (LHsExpr GhcTc, LHsExpr GhcTc)
application expression = case stripTyped (unLoc expression) of
  HsApp _ function argument -> pure (function, argument)
  _ -> throwIO UnprovedRootContinuation

neutralTerminal :: SegmentOperations -> HsExpr GhcTc -> Bool
neutralTerminal operations expression = case stripTyped expression of
  HsApp _ function argument
    | HsVar _ identifier <- stripTyped (unLoc function)
    , unLoc identifier == segmentPureId operations
    , isUnit (stripTyped (unLoc argument)) -> True
  _ -> False
  where
    isUnit (ExplicitTuple _ [] Boxed) = True
    isUnit (XExpr (ConLikeTc constructor _ _)) = constructor == RealDataCon unitDataCon
    isUnit _ = False

lazyTypedVariable :: LPat GhcTc -> Maybe Id
lazyTypedVariable pattern = case unLoc pattern of
  ParPat _ inner -> lazyTypedVariable inner
  LazyPat _ inner -> variable inner
  _ -> Nothing
  where
    variable inner = case unLoc inner of
      VarPat _ identifier -> Just (unLoc identifier)
      ParPat _ parenthesized -> variable parenthesized
      _ -> Nothing

stripTyped :: HsExpr GhcTc -> HsExpr GhcTc
stripTyped expression = case expression of
  HsPar _ inner -> stripTyped (unLoc inner)
  XExpr (WrapExpr _ inner) -> stripTyped inner
  XExpr (ExpandedThingTc _ inner) -> stripTyped inner
  other -> other

exportedIds :: HsLocalBinds GhcTc -> [Id]
exportedIds locals = nub (concatMap (collectHsBindBinders CollNoDictBinders)
  (collect locals :: [HsBind GhcTc]))

collect :: (Data value, Typeable selected) => value -> [selected]
collect value = case cast value of
  Just selected -> [selected]
  Nothing -> concat (gmapQ collect value)

occurrence :: Id -> String
occurrence = occNameString . nameOccName . idName

unique :: TypedSegmentFailure -> TypedSegmentFailure -> [value] -> IO value
unique missing ambiguous values = case values of
  [value] -> pure value
  [] -> throwIO missing
  _ -> throwIO ambiguous
