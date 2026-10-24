module Tidepool.Introspection
  ( InspectionResult (..),
    InfoEntry (..),
    TypeMatch (..),
    TypeMatchQuality (..),
    Availability (..),
    IdentifierNamespace (..),
    IdentifierRef (..),
    ScopeProvenance (..),
    TypeExpression (..),
    TypeInfo (..),
    FieldInfo (..),
    ConstructorInfo (..),
    ClassMethodInfo (..),
    DeclarationInfo (..),
    IdentifierInfo (..),
    StructuredQueryError (..),
    normalizeInspectionWildcards,
    searchTypeMatches,
    InspectionWork, withInspectionWork, inspectionCompilationWork, runInspection, runInspectionGhc, runWorldInspectionGhc,
    encodeInspectionResults,
    inspectPublicModuleExports,
    isCleanModuleLookupMiss,
    boundedDistinctNames,
  )
where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Control.Monad (foldM, forM, forM_)
import Control.Exception (throwIO)
import Control.Monad.IO.Class (MonadIO, liftIO)
import Control.Monad.State.Strict (State, evalState, get, put)
import Data.ByteString qualified as BS
import Data.Generics (everything, everywhereM, mkM, mkQ)
import Data.List (foldl', isPrefixOf, nub, nubBy, sortOn)
import Data.Map.Strict qualified as Map
import Data.Maybe (catMaybes, isJust, listToMaybe)
import GHC.Core.TyCo.FVs (tyCoVarsOfTypes, tyConsOfType)
import GHC.Core.TyCo.Subst (emptySubst, extendTvSubst)
import GHC.Core.TyCo.Tidy (tidyOpenType)
import GHC.Core.Type (getTyVar_maybe, splitTyConApp_maybe, substTy)
import GHC.Tc.Solver (tcCheckWanteds)
import GHC.Tc.Solver.InertSet (emptyInert)
import GHC.Tc.Types (TcGblEnv, tcg_mod, tcg_type_env, tcg_exports)
import GHC.Types.Avail (availNames)
import GHC.Tc.Utils.Monad (initTcWithGbl)
import GHC.Types.SrcLoc (mkRealSrcSpan, mkRealSrcLoc)
import GHC.Data.FastString (mkFastString)
import GHC.Types.Var.Set (isEmptyVarSet)
import Data.Set qualified as Set
import Data.Text qualified as T
import Data.Word (Word32, Word64)
import GHC
import GHC.Core.Class (classTyVars)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.ConLike (ConLike (..), isVanillaConLike)
import GHC.Core.DataCon (dataConDisplayType, dataConFieldType, dataConOrigArgTys)
import GHC.Core.Multiplicity (scaledThing)
import GHC.Core.TyCon (isAlgTyCon)
import GHC.Core.Unify (tcMatchTy)
import GHC.Iface.Type (AltPpr (..), ShowForAllFlag (..), ShowHowMuch (..), ShowSub (..))
import GHC.Types.Id (idName)
import GHC.Types.Name (nameIsLocalOrFrom, nameModule_maybe, nameOccName)
import GHC.Types.TypeEnv (lookupTypeEnv, typeEnvIds)
import GHC.Types.Name (isDataConName, isTyConName, isVarName)
import GHC.Types.Name.Occurrence (fieldOcc_maybe, isSymOcc, mkTyVarOcc, mkVarOcc, mkTcOcc, mkDataOcc, occNameString)
import GHC.Types.Name.Reader (GlobalRdrEnv, RdrName (..), globalRdrEnvElts, greName, greRdrNames, mkRdrUnqual, rdrNameOcc, lookupGRE, LookupGRE(..), WhichGREs(..), FieldsOrSelectors(..))
import GHC.Types.TyThing (tyThingParent_maybe)
import GHC.Unit.Module (moduleUnit)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Types.PkgQual (PkgQual(..))
import GHC.Unit.Types (unitString)
import GHC.Types.TyThing.Ppr (pprTyThing, pprTyThingInContext)
import GHC.Types.FieldLabel (flLabel, flSelector)
import GHC.Types.Var (varName)
import GHC.Types.Var.Env (emptyTidyEnv)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Tc.Utils.TcType (tcSplitFunTys, tcSplitSigmaTy)
import GHC.Utils.Outputable
  ( Outputable,
    defaultSDocContext,
    ppr,
    renderWithContext,
    sdocSuppressUniques,
  )
import Tidepool.ExtractRequest
  ( InspectionProvenance (..), InspectionRequest (..), StructuredInspection (..),
    ExactNameInspection(..),
    StructuredNameNamespace (..), StructuredNameScope (..)
  )
import GHC.Driver.Monad (reflectGhc, Session(..))
import Data.IORef (IORef, newIORef, readIORef, modifyIORef')
import Tidepool.Timing (readTimingEnabled, emitCount)
import Tidepool.ExactHydration (forkExactContext)
import Tidepool.Session (isReservedSessionModuleName)

data InfoEntry = InfoEntry
  { infoName :: String,
    infoNominalName :: Name,
    infoNamespace :: IdentifierNamespace,
    infoModule :: Maybe String,
    infoKind :: String,
    infoDisplay :: String,
    infoAvailability :: Availability,
    infoReferences :: [IdentifierRef]
  }
  deriving (Eq)

-- The compiler-owned nominal Name is selection evidence, not debug output.
instance Show InfoEntry where
  show entry = "InfoEntry " ++ show
    (infoName entry, infoNamespace entry, infoModule entry, infoKind entry,
      infoDisplay entry, infoAvailability entry, infoReferences entry)

data InspectionResult
  = InspectionType String String Availability
  | InspectionRowFit IdentifierRef Availability
  | InspectionInfo String [InfoEntry]
  | InspectionAmbiguous String [InfoEntry]
  | InspectionNotFound String
  | InspectionModuleNotFound String [StructuredNameScope]
  | InspectionRejected String
  | InspectionBrowse String Bool [InfoEntry]
  | InspectionTypeMatches String [TypeMatch]
  | InspectionStructuredInfo IdentifierInfo
  | InspectionStructuredType TypeInfo
  | InspectionStructuredError StructuredQueryError
  deriving (Eq, Show)

data TypeMatchQuality
  = TypeMatchExact
  | TypeMatchUsable
  deriving (Eq, Ord, Show)

-- | How a callable relates to the asking actor's effect row.
--
-- `Polymorphic` means the row fits and every closed constraint solves, but some
-- constraint stays open because the call site decides it. That is a usable
-- name, not an uncertain one, and it used to be reported as `Unknown`.
--
-- The constructor order is the ranking: `Ord` is the sort key for type
-- search, so a new constructor's position is behaviour. The wire value is
-- the constructor name itself.
data Availability = Available | Polymorphic | Unknown | Unavailable
  deriving (Eq, Ord, Show)

data TypeMatch = TypeMatch
  { typeMatchName :: String,
    typeMatchNominalName :: Name,
    typeMatchNamespace :: IdentifierNamespace,
    typeMatchModule :: Maybe String,
    typeMatchSignature :: String,
    typeMatchQuality :: TypeMatchQuality,
    typeMatchAvailability :: Availability,
    typeMatchReferences :: [IdentifierRef]
  }
  deriving (Eq)

instance Show TypeMatch where
  show entry = "TypeMatch " ++ show
    (typeMatchName entry, typeMatchNamespace entry, typeMatchModule entry,
      typeMatchSignature entry, typeMatchQuality entry,
      typeMatchAvailability entry, typeMatchReferences entry)

data IdentifierNamespace
  = ValueIdentifier
  | TypeIdentifier
  | ConstructorIdentifier
  | FieldIdentifier
  deriving (Eq, Ord, Show)

data IdentifierRef = IdentifierRef
  { identifierModule :: String,
    identifierName :: String,
    identifierNamespace :: IdentifierNamespace
  }
  deriving (Eq, Ord, Show)

data ScopeProvenance = ScopeProvenance
  { provenanceScope :: StructuredNameScope,
    provenanceGeneration :: Word64,
    provenanceFingerprint :: String
  }
  deriving (Eq, Show)

data TypeExpression = TypeExpression
  { typeCanonical :: String,
    typeVariables :: [String],
    typeConstraints :: [String]
  }
  deriving (Eq, Show)

data TypeInfo = TypeInfo
  { typeIdentifier :: IdentifierRef,
    typeExpression :: TypeExpression,
    typeProvenance :: ScopeProvenance
  }
  deriving (Eq, Show)

data FieldInfo = FieldInfo
  { fieldName :: String,
    fieldType :: TypeExpression
  }
  deriving (Eq, Show)

data ConstructorInfo = ConstructorInfo
  { constructorRef :: IdentifierRef,
    constructorType :: TypeExpression,
    constructorArguments :: [TypeExpression],
    recordFields :: [FieldInfo]
  }
  deriving (Eq, Show)

data ClassMethodInfo = ClassMethodInfo
  { classMethodRef :: IdentifierRef,
    classMethodType :: TypeExpression
  }
  deriving (Eq, Show)

data DeclarationInfo
  = ValueDeclaration TypeExpression
  | DataDeclaration [String] [ConstructorInfo]
  | NewtypeDeclaration [String] ConstructorInfo
  | TypeSynonymDeclaration [String] TypeExpression
  | ClassDeclaration [String] [TypeExpression] [ClassMethodInfo]
  | ConstructorDeclaration IdentifierRef ConstructorInfo
  | RecordSelectorDeclaration IdentifierRef TypeExpression
  deriving (Eq, Show)

data IdentifierInfo = IdentifierInfo
  { inspectedIdentifier :: IdentifierRef,
    identifierDeclaration :: DeclarationInfo,
    identifierParent :: Maybe IdentifierRef,
    identifierProvenance :: ScopeProvenance
  }
  deriving (Eq, Show)

data StructuredQueryError
  = StructuredUnknown StructuredInspection
  | StructuredAmbiguous StructuredInspection [IdentifierRef]
  | StructuredUnknownModule String
  | StructuredUnsupported String
  deriving (Eq, Show)

data AvailabilityContext = AvailabilityContext
  { availabilityHscEnv :: HscEnv,
    availabilityTcGblEnv :: TcGblEnv,
    availabilityEffTyCon :: Maybe TyCon,
    availabilityRow :: Maybe Type,
    availabilityWork :: Maybe (IORef InspectionWorkCounts)
  }

newtype InspectionWork = InspectionWork (Maybe (IORef InspectionWorkCounts))

data InspectionWorkCounts = InspectionWorkCounts
  { workResolvedNames :: !Integer, workRichEntries :: !Integer, workAvailabilityChecks :: !Integer, workConstraintSolves :: !Integer, workCompileAttempts :: !Integer }

-- One physical request may inspect several separately checked environments.
-- Its observation lifetime therefore belongs outside the per-environment call.
withInspectionWork :: (InspectionWork -> IO a) -> IO a
withInspectionWork action = do
  enabled <- readTimingEnabled
  work <- if enabled then Just <$> newIORef (InspectionWorkCounts 0 0 0 0 0) else pure Nothing
  result <- action (InspectionWork work)
  forM_ work $ \counts -> do
    observed <- readIORef counts
    emitCount True "inspection.resolved_names" (workResolvedNames observed)
    emitCount True "inspection.rich_entries" (workRichEntries observed)
    emitCount True "inspection.availability_checks" (workAvailabilityChecks observed)
    emitCount True "inspection.constraint_solves" (workConstraintSolves observed)
    emitCount True "inspection.compile_attempts" (workCompileAttempts observed)
  pure result

-- Count at the existing production compile callback, including authored failures.
inspectionCompilationWork :: InspectionWork -> IO ()
inspectionCompilationWork (InspectionWork work) = forM_ work $ \counts ->
  modifyIORef' counts (\observed -> observed { workCompileAttempts = workCompileAttempts observed + 1 })

observeWork :: MonadIO m => AvailabilityContext -> (InspectionWorkCounts -> InspectionWorkCounts) -> m ()
observeWork context update = liftIO $ case availabilityWork context of
  Nothing -> pure ()
  Just counts -> modifyIORef' counts update

resolveNameWork :: MonadIO m => AvailabilityContext -> m ()
resolveNameWork context = observeWork context (\counts -> counts { workResolvedNames = workResolvedNames counts + 1 })

richEntryWork :: MonadIO m => AvailabilityContext -> m ()
richEntryWork context = observeWork context (\counts -> counts { workRichEntries = workRichEntries counts + 1 })

-- | The sentinel is generated with a qualified Data.Proxy signature. Its
-- argument is inspected as a GHC Type; rendered text never determines a row.
inspectionExecutionRow :: (GhcMonad m) => GlobalRdrEnv -> m (Maybe Type)
inspectionExecutionRow rdrEnv = do
  let names =
        [ greName gre
        | gre <- globalRdrEnvElts rdrEnv,
          any (matchesQuery "__tidepool_inspection_row") (greRdrNames gre)
        ]
  case nubBy (==) names of
    [name] -> do
      found <- lookupName name
      case found of
        Just (AnId identifier) -> Just <$> inspectionRow identifier
        _ -> malformed
    _ -> pure Nothing
  where
    malformed = liftIO (ioError (userError "inspection row sentinel has no typed Proxy row"))

-- | Read the actor row from the exact target-local Id retained by the checked
-- source. Its interface is deliberately not installed merely so inspection
-- can rediscover a compiler-only binding by name.
inspectionRow :: (GhcMonad m) => Id -> m Type
inspectionRow identifier = do
  proxy <- lookupProxyTyCon
  eff <- lookupEffTyCon
  let (_, _, body) = tcSplitSigmaTy (idType identifier)
  case splitTyConApp_maybe body of
    Just (constructor,[row,_]) | Just constructor == eff -> pure row
    Just (constructor,arguments) | Just constructor == proxy
        , Just row <- listToMaybe (reverse arguments) -> pure row
    _ -> malformed
  where
    malformed = liftIO (ioError (userError "inspection row sentinel lacks an exact Proxy or Eff row signature"))

lookupProxyTyCon :: (GhcMonad m) => m (Maybe TyCon)
lookupProxyTyCon = do
  mdl <- findModule (mkModuleName "Data.Proxy") Nothing
  info <- getModuleInfo mdl
  let names = maybe [] modInfoExports info
  case [name | name <- names, occNameString (nameOccName name) == "Proxy"] of
    name : _ -> do
      found <- lookupName name
      pure $ case found of
        Just (ATyCon constructor) -> Just constructor
        _ -> Nothing
    [] -> pure Nothing

lookupEffTyCon :: (GhcMonad m) => m (Maybe TyCon)
lookupEffTyCon = handleSourceError (\_ -> pure Nothing) $ do
  mdl <- findModule (mkModuleName "Control.Monad.Freer") Nothing
  info <- getModuleInfo mdl
  let names = maybe [] modInfoExports info
  case [name | name <- names, occNameString (nameOccName name) == "Eff"] of
    name : _ -> do
      found <- lookupName name
      case found of
        Just (ATyCon constructor) -> pure (Just constructor)
        _ -> missing
    [] -> missing
  where
    missing = pure Nothing

-- | Only the output Eff row is rooted in the actor's execution row. Input
-- Eff rows (for example on Branch or Unfold) can remain polymorphic.
rootEffRow :: Maybe TyCon -> Type -> Maybe Type
rootEffRow maybeEffTyCon ty = do
  effTyCon <- maybeEffTyCon
  case splitTyConApp_maybe (snd (tcSplitFunTys ty)) of
    Just (constructor, row : _) | constructor == effTyCon -> Just row
    _ -> Nothing

availabilityFor :: AvailabilityContext -> Maybe Type -> [Type] -> IO Availability
availabilityFor context output predicates = do
  observeWork context (\counts -> counts { workAvailabilityChecks = workAvailabilityChecks counts + 1 })
  let outputRow = output >>= rootEffRow (availabilityEffTyCon context)
      rowDecision = case (availabilityRow context, outputRow) of
        (_, Nothing) -> Available
        (Nothing, Just _) -> Unknown
        (Just actual, Just candidate) ->
          case getTyVar_maybe candidate of
            Just _ -> Available
            Nothing
              | eqType actual candidate -> Available
              | isEmptyVarSet (tyCoVarsOfTypes [candidate]) -> Unavailable
              | otherwise -> Unknown
      finalPredicates = case (availabilityRow context, outputRow >>= getTyVar_maybe) of
        (Just actual, Just rowVariable) ->
          map (substTy (extendTvSubst emptySubst rowVariable actual)) predicates
        _ -> predicates
      closed = filter (isEmptyVarSet . tyCoVarsOfTypes . pure) finalPredicates
  allSolved <- solvePredicates context finalPredicates
  closedSolved <- if allSolved
    then pure True
    else if length closed == length finalPredicates
      then pure False
      else solvePredicates context closed
  -- A predicate that is open only because it is polymorphic is not evidence
  -- against a callable: the call site supplies the instantiation and discharges
  -- it. `R.start` and `R.client` read `unknown` for exactly this reason while
  -- the shipped guidance called them available in every cell, and both worked
  -- first time. Say which of the two it is rather than collapsing them.
  let openPredicates = length finalPredicates - length closed
  pure $ if rowDecision == Unavailable || not closedSolved
    then Unavailable
    else if allSolved && rowDecision == Available
      then Available
      else if rowDecision == Available && openPredicates > 0
        then Polymorphic
        else Unknown

solvePredicates :: AvailabilityContext -> [Type] -> IO Bool
solvePredicates context predicates =
  if null predicates
    then pure True
    else do
      observeWork context (\counts -> counts { workConstraintSolves = workConstraintSolves counts + 1 })
      (_, result) <- initTcWithGbl (availabilityHscEnv context) (availabilityTcGblEnv context)
        (mkRealSrcSpan (mkRealSrcLoc (mkFastString "<inspection availability>") 1 1)
          (mkRealSrcLoc (mkFastString "<inspection availability>") 1 1))
        (tcCheckWanteds emptyInert predicates)
      maybe (ioError (userError "inspection constraint solver failed")) pure result

signatureAvailability :: AvailabilityContext -> Type -> IO Availability
signatureAvailability context signature = do
  let (_, predicates, body) = tcSplitSigmaTy signature
  availabilityFor context (Just body) predicates

-- | Replace each parsed anonymous type wildcard with a distinct implicit type
-- variable. GHC then kind-checks and quantifies those variables normally.
-- Rewriting the parsed tree preserves qualification and repeated named
-- variables; rendered source never drives this transformation.
normalizeInspectionWildcards :: ParsedSource -> ParsedSource
normalizeInspectionWildcards source =
  let occupied =
        Set.fromList $
          everything (++) (mkQ [] (\name -> [occNameString (rdrNameOcc name)])) source
   in evalState (everywhereM (mkM replaceWildcard) source) (0, occupied)
  where
    replaceWildcard ::
      HsType GhcPs ->
      State (Int, Set.Set String) (HsType GhcPs)
    replaceWildcard (HsWildCardTy _) = do
      (index, occupied) <- get
      let (nextIndex, name) = freshName occupied index
      put (nextIndex, Set.insert name occupied)
      pure $
        HsTyVar
          noAnn
          NotPromoted
          (noLocA (mkRdrUnqual (mkTyVarOcc name)))
    replaceWildcard other = pure other

    freshName occupied index =
      let candidate = "__inspection_w" ++ show index
       in if Set.member candidate occupied
            then freshName occupied (index + 1)
            else (index + 1, candidate)

-- | Match a checked probe type against every value in the exact reader
-- environment of the inspection module. Matching is entirely in memory:
-- callers compile the query once, never once per candidate.
searchTypeMatches :: (GhcMonad m) => HscEnv -> TcGblEnv -> GlobalRdrEnv -> Name -> Type -> m [TypeMatch]
searchTypeMatches hscEnv tcGblEnv rdrEnv queryBinder query = do
  row <- inspectionExecutionRow rdrEnv
  effTyCon <- lookupEffTyCon
  case (row, effTyCon) of
    (Just _, Nothing) -> liftIO (ioError (userError "inspection could not resolve Eff type constructor"))
    _ -> pure ()
  searchTypeMatchesWithContext (AvailabilityContext hscEnv tcGblEnv effTyCon row Nothing) rdrEnv queryBinder query

inspectionLookupName :: GhcMonad m => AvailabilityContext -> Name -> m (Maybe TyThing)
inspectionLookupName context name = case lookupTypeEnv (tcg_type_env checked) name of
  Just thing | nameIsLocalOrFrom (tcg_mod checked) name -> pure (Just thing)
  _ -> lookupName name
  where checked = availabilityTcGblEnv context

-- GHC.lookupName discards lookup diagnostics. An advertised Name without its
-- declaration is unavailable information, not evidence that the name is absent.
requireLookupName :: GhcMonad m => Name -> m (Maybe TyThing) -> m TyThing
requireLookupName name action = action >>= maybe
  (liftIO (ioError (userError ("inspection declaration information unavailable for " ++ render name)))) pure

searchTypeMatchesWithContext :: (GhcMonad m) => AvailabilityContext -> GlobalRdrEnv -> Name -> Type -> m [TypeMatch]
searchTypeMatchesWithContext context rdrEnv queryBinder query = do
  let entries =
        filter (\entry -> greName entry /= queryBinder && visibleInspectionName (greName entry)) (globalRdrEnvElts rdrEnv)
      spellingOwners =
        Map.fromListWith Set.union
          [ (spelling, Set.singleton (greName entry))
          | entry <- entries,
            (_, spelling) <- sourceSpellings entry
          ]
      namedEntries =
        nubBy (\(left, _) (right, _) -> greName left == greName right) $
          catMaybes
            [ fmap (\spelling -> (entry, spelling)) (usableSpelling spellingOwners entry)
            | entry <- entries
            ]
  things <- fmap catMaybes $ forM namedEntries $ \(entry, spelling) -> do
    let name = greName entry
    found <- inspectionLookupName context name
    pure $ case found of
      Just (AnId identifier) -> Just (name, spelling, identifier)
      _ -> Nothing
  matches <- forM things $ \(name, spelling, identifier) -> do
    let candidate = idType identifier
    case matchQuality query candidate of
      Nothing -> pure Nothing
      Just (quality, substitution) -> do
        let (_, predicates, body) = tcSplitSigmaTy candidate
            specialized = maybe id substTy substitution
        availability <- liftIO $ availabilityFor context
          (Just (specialized body)) (map specialized predicates)
        pure (Just (toMatch name spelling identifier quality availability))
  pure . sortOn matchKey $ catMaybes matches
  where
    sourceSpellings entry =
      [ (priority, expressionSpelling rdrName)
      | rdrName <- greRdrNames entry,
        priority <- case rdrName of
          Unqual _ -> [0 :: Int]
          Qual _ _ -> [1]
          Orig _ _ -> []
          Exact _ -> []
      ]

    -- Return an expression that can be applied, including qualified operators.
    expressionSpelling rdrName =
      let spelling = renderWithContext defaultSDocContext (ppr rdrName)
       in if isSymOcc (rdrNameOcc rdrName)
            then "(" ++ spelling ++ ")"
            else spelling

    usableSpelling owners entry =
      snd
        <$> listToMaybe
          [ candidate
          | candidate@(_, spelling) <-
              sortOn
                (\(priority, rendered) -> (priority, length rendered, rendered))
                (sourceSpellings entry),
            Map.lookup spelling owners == Just (Set.singleton (greName entry))
          ]

    matchQuality expected candidate
      | eqType expected candidate = Just (TypeMatchExact, Nothing)
      | otherwise = fmap (\matched -> (TypeMatchUsable, Just matched))
          (matchesEitherDirection expected candidate)

    matchesEitherDirection left right =
      let (_, _, leftBody) = tcSplitSigmaTy left
          (_, _, rightBody) = tcSplitSigmaTy right
       in tcMatchTy rightBody leftBody
            `mplusMatch` (emptySubst <$ tcMatchTy leftBody rightBody)

    mplusMatch first second = case first of
      Just matched -> Just matched
      Nothing -> second

    toMatch name spelling identifier quality availability =
      TypeMatch
        { typeMatchName = spelling,
          typeMatchNominalName = name,
          typeMatchNamespace = if isRecordSelector identifier then FieldIdentifier else ValueIdentifier,
          typeMatchModule = moduleNameString . moduleName <$> nameModule_maybe name,
          typeMatchSignature =
            renderType (idType identifier),
          typeMatchQuality = quality,
          typeMatchAvailability = availability,
          typeMatchReferences = typeReferences (idType identifier)
        }

    matchKey result =
      ( typeMatchAvailability result,
        typeMatchQuality result,
        typeMatchName result,
        typeMatchModule result,
        typeMatchSignature result
      )

runInspection ::
  HscEnv ->
  TcGblEnv ->
  GlobalRdrEnv ->
  Map.Map String Id ->
  [InspectionRequest] ->
  IO [InspectionResult]
runInspection hscEnv tcGblEnv rdrEnv inspectionProbes requests = withInspectionWork $ \work -> do
  session <- Session <$> newIORef hscEnv
  reflectGhc (runInspectionGhc work hscEnv tcGblEnv rdrEnv inspectionProbes requests) session

runInspectionGhc :: InspectionWork -> HscEnv -> TcGblEnv -> GlobalRdrEnv -> Map.Map String Id
  -> [InspectionRequest] -> Ghc [InspectionResult]
runInspectionGhc work hscEnv tcGblEnv rdrEnv inspectionProbes =
  runWorldInspectionGhc work hscEnv tcGblEnv rdrEnv inspectionProbes Map.empty

-- Exact aliases come from the held World's admitted value-interface owner.
-- They describe original compiler Names; they confer no runtime value access.
runWorldInspectionGhc :: InspectionWork -> HscEnv -> TcGblEnv -> GlobalRdrEnv -> Map.Map String Id
  -> Map.Map String Id -> [InspectionRequest] -> Ghc [InspectionResult]
runWorldInspectionGhc (InspectionWork work) hscEnv tcGblEnv rdrEnv inspectionProbes aliases requests = do
  borrowed <- liftIO (forkExactContext hscEnv)
  setSession borrowed
  effTyCon <- lookupEffTyCon
  row <- case Map.lookup "__tidepool_inspection_row" inspectionProbes of
    Just identifier -> Just <$> inspectionRow identifier
    Nothing -> pure Nothing
  case (row,effTyCon) of
    (Just _,Nothing) -> liftIO (ioError (userError "inspection could not resolve Eff type constructor"))
    _ -> pure ()
  let context = AvailabilityContext borrowed tcGblEnv effTyCon row work
  results <- reverse . snd <$> foldM (inspect context) (0 :: Int,[]) requests
  pure (zipWith finalize requests results)
  where
    inspect context (typeIndex, results) request = case request of
      InspectTypeOf expression ->
        let binder = "__tidepool_inspect_" ++ show typeIndex
         in case Map.lookup binder inspectionProbes of
              Just identifier -> do
                availability <- liftIO $ signatureAvailability context (idType identifier)
                let display = renderType (idType identifier)
                pure (typeIndex + 1, InspectionType expression display availability : results)
              Nothing -> missing binder
      InspectNameInfo query -> do
        result <- inspectName context rdrEnv query
        pure (typeIndex, result : results)
      InspectScope -> do
        let names = nub [name | gre <- globalRdrEnvElts rdrEnv, let name = greName gre,
              visibleInspectionName name]
        entries <- browseEntries context True names
        pure (typeIndex, InspectionBrowse "" True
          (sortOn (\entry -> (infoModule entry, infoName entry, infoKind entry)) entries) : results)
      InspectModule moduleName expanded -> do
        result <- inspectModule context moduleName expanded Nothing
        pure (typeIndex, result : results)
      InspectModuleBounded moduleName expanded budget -> do
        result <- inspectModule context moduleName expanded (Just budget)
        pure (typeIndex, result : results)
      InspectTypeSearch query -> do
        result <- inspectTypeSearch context rdrEnv inspectionProbes Nothing query
        pure (typeIndex, result : results)
      InspectTypeSearchBounded query _ -> do
        result <- inspectTypeSearch context rdrEnv inspectionProbes Nothing query
        pure (typeIndex, result : results)
      InspectStructuredInfoOf query -> do
        result <- structured context StructuredInfo query
        pure (typeIndex, result : results)
      InspectStructuredTypeOf query -> do
        result <- structured context StructuredType query
        pure (typeIndex, result : results)
      InspectKeyedAvailability query _ | structuredScope query /= StructuredCurrentScope
          || structuredNamespace query == StructuredAnyName ->
        pure (typeIndex,InspectionStructuredError (StructuredUnsupported "keyed row fit requires the current scope and a concrete namespace"):results)
      InspectKeyedAvailability query owner -> do
        let aliasCandidates = [idName identifier | identifier <- Map.elems aliases
              , let name = idName identifier
              , let ref = identifierRef name (AnId identifier)
              , namespaceMatches (structuredNamespace query) name
              , identifierModule ref == owner && identifierName ref == structuredName query]
            -- A canonical reference does not carry its imported spelling.
            -- Query the existing occurrence bucket, including qualified-only
            -- imports, then require its exact actual namespace/module below.
            candidates = boundedDistinctNames Nothing
              (filter relevant (map canonicalName (aliasCandidates ++ canonicalReferenceNames rdrEnv query)))
            relevant name = maybe "" (moduleNameString . moduleName) (nameModule_maybe name) == owner
              && occNameString (nameOccName name) == structuredName query
              && namespaceMatches (structuredNamespace query) name
        things <- fmap catMaybes $ forM candidates $ \name -> do
          thing <- requireLookupName name $ case Map.lookup name aliasNames of
            Just identifier -> pure (Just (AnId identifier))
            Nothing -> inspectionLookupName context name
          pure $ if exactThingMatches (structuredNamespace query) thing
            then Just (identifierRef name thing,thing) else Nothing
        let requested = IdentifierRef owner (structuredName query) (case structuredNamespace query of
              StructuredTypeName -> TypeIdentifier
              StructuredConstructorName -> ConstructorIdentifier
              StructuredFieldName -> FieldIdentifier
              _ -> ValueIdentifier)
        -- Candidates are distinct actual canonical Names. Rendering can erase
        -- unit/field identity, so public IdentifierRefs cannot decide uniqueness.
        result <- case things of
          [] -> pure (InspectionRowFit requested Unknown)
          [(ref,thing)] -> InspectionRowFit ref <$> thingAvailability context thing
          _ | structuredNamespace query == StructuredFieldName ->
            pure (InspectionStructuredError (StructuredUnsupported "canonical field reference is ambiguous"))
          many -> pure (InspectionStructuredError (StructuredAmbiguous query (map fst many)))
        pure (typeIndex,result:results)
    structured context mode query = do
      NameSelection _ selection <- namesInScope tcGblEnv rdrEnv
        (ExactNameInspection (structuredScope query) (structuredNamespace query) (structuredName query))
      case selection of
        Left missingModule -> pure (InspectionStructuredError (StructuredUnknownModule missingModule))
        Right (names,visible) -> do
          things <- fmap catMaybes $ forM names $ \name -> do
            -- Projection follows actual namespace/winner selection and consumes
            -- only an exact checked local alias Name from this target.
            selected@(_,thing) <- case Map.lookup name checkedAliases of
              Just original -> pure (idName original,AnId original)
              Nothing -> (name,) <$> requireLookupName name (inspectionLookupName context name)
            pure $ if exactThingMatches (structuredNamespace query) thing then Just selected else Nothing
          pure $ case distinctOn fst things of
            [] -> InspectionStructuredError (StructuredUnknown query)
            [thing] -> inspectResolved mode query visible thing
            many -> InspectionStructuredError (StructuredAmbiguous query (map (uncurry identifierRef) many))
    finalize request result = case canonicalize result of
      InspectionTypeMatches query matches ->
        let distinct = distinctOn typeMatchNominalName matches
        in InspectionTypeMatches query (case request of
          InspectTypeSearchBounded _ limit -> boundedNames (Just limit) distinct
          _ -> distinct)
      answer -> answer
    checkedAliases = Map.fromList
      [(idName identifier,original) | identifier <- typeEnvIds (tcg_type_env tcGblEnv)
        , let name = idName identifier
        , isVarName name
        , nameModule_maybe name == Just (tcg_mod tcGblEnv)
        , Just original <- [Map.lookup (occNameString (nameOccName name)) aliases]]
    canonicalName name = maybe name idName (Map.lookup name checkedAliases)
    aliasNames = Map.fromList [(idName identifier,identifier) | identifier <- Map.elems aliases]
    canonicalize result = case result of
      InspectionTypeMatches query matches -> InspectionTypeMatches query (map canonicalMatch matches)
      answer -> answer
    canonicalMatch match = case Map.lookup (typeMatchNominalName match) checkedAliases of
      Just identifier ->
        match { typeMatchNominalName = idName identifier
              , typeMatchNamespace = if isRecordSelector identifier then FieldIdentifier else ValueIdentifier
              , typeMatchModule = moduleNameString . moduleName <$> nameModule_maybe (idName identifier) }
      Nothing -> match
    missing binder = liftIO (ioError (userError ("inspection module did not expose " ++ binder)))

-- The budget bounds the encoded match list, not the search itself. Matching
-- every candidate is required for the answer to be complete, so the work stays
-- proportional to the environment even when the wire list is capped.
inspectTypeSearch :: (GhcMonad m) => AvailabilityContext -> GlobalRdrEnv -> Map.Map String Id -> Maybe Int -> String -> m InspectionResult
inspectTypeSearch context rdrEnv inspectionProbes budget query =
  case Map.lookup binderName inspectionProbes of
    Just identifier ->
      (InspectionTypeMatches query . boundedNames budget) <$> searchTypeMatchesWithContext context rdrEnv
        (idName identifier) (idType identifier)
    Nothing -> liftIO (ioError (userError ("inspection module did not expose " ++ binderName)))
  where
    binderName = "__tidepool_type_search_query"

data StructuredMode = StructuredInfo | StructuredType


-- The service has already resolved the queried module in its held world.
-- Selection uses that module's public exports; provenance follows the actual
-- defining Name, which may belong to a reexported module or unit.
inspectPublicModuleExports :: GhcMonad m => Bool -> StructuredInspection -> [Name]
  -> m (InspectionResult,Maybe Module)
inspectPublicModuleExports typeOnly query exports =
  inspectStructuredCandidates (if typeOnly then StructuredType else StructuredInfo) query visible
    [name | name <- visible, matchesNameQuery (structuredName query) name
      && namespaceMatches (structuredNamespace query) name]
  where visible = nub exports

inspectStructuredCandidates :: GhcMonad m => StructuredMode -> StructuredInspection -> [Name] -> [Name]
  -> m (InspectionResult,Maybe Module)
inspectStructuredCandidates mode query visibleNames names = do
  things <- fmap catMaybes $ forM names $ \name -> do
    thing <- requireLookupName name (lookupName name)
    pure $ if exactThingMatches (structuredNamespace query) thing then Just (name,thing) else Nothing
  pure $ case things of
    [] -> (InspectionStructuredError (StructuredUnknown query),Nothing)
    [resolved@(name,_)] -> (inspectResolved mode query visibleNames resolved,nameModule_maybe name)
    many -> (InspectionStructuredError (StructuredAmbiguous query (map (uncurry identifierRef) many)),Nothing)

-- Selection and attempted-scope evidence are issued by the same search.
data NameSelection = NameSelection [StructuredNameScope] (Either String ([Name], [Name]))

namesInScope :: GhcMonad m => TcGblEnv -> GlobalRdrEnv -> ExactNameInspection -> m NameSelection
namesInScope checked rdrEnv query = case exactNameScope query of
  StructuredCurrentScope -> do
    let entries = globalRdrEnvElts rdrEnv
        visible = nub (map greName entries)
        selected = nub [greName entry | entry <- entries
          , namespaceMatches (exactNameNamespace query) (greName entry)
          , any (matchesQuery (exactNameQuery query)) (greRdrNames entry)]
    pure (NameSelection [StructuredCurrentScope] (Right (selected,visible)))
  scope -> do
    (scopes, result) <- visibleNamesInScope checked rdrEnv scope
    pure (NameSelection scopes (fmap (\visible -> (filter matches visible, visible)) result))
  where matches name = matchesNameQuery (exactNameQuery query) name
          && namespaceMatches (exactNameNamespace query) name

canonicalReferenceNames :: GlobalRdrEnv -> StructuredInspection -> [Name]
canonicalReferenceNames environment query = map greName (lookupGRE environment lookup)
  where
    occurrence constructor = constructor (structuredName query)
    lookup = case structuredNamespace query of
      StructuredTypeName -> LookupOccName (occurrence mkTcOcc) SameNameSpace
      StructuredConstructorName -> LookupOccName (occurrence mkDataOcc) SameNameSpace
      _ -> LookupOccName (occurrence mkVarOcc) (RelevantGREs WantBoth True True)

-- Scope observation is shared by exact selection and discovery inventory.
-- It issues the names and actual attempts; selecting a projection grants no
-- additional module/source authority.
visibleNamesInScope :: GhcMonad m => TcGblEnv -> GlobalRdrEnv -> StructuredNameScope -> m ([StructuredNameScope], Either String [Name])
visibleNamesInScope checked rdrEnv scope = case scope of
  StructuredCurrentScope -> pure ([StructuredCurrentScope], Right (nub (map greName (globalRdrEnvElts rdrEnv))))
  StructuredPublicModule requested
    | isReservedSessionModuleName requested -> pure ([], Left requested)
    | otherwise -> do
        resolved <- publicModuleExports checked requested
        pure ([scope], maybe (Left requested) (Right . nub . snd) resolved)

-- Share the held-world resolver's absence contract. Hidden, unusable and
-- ambiguous providers cannot establish absence, even without a package choice.
isCleanModuleLookupMiss :: FindResult -> Bool
isCleanModuleLookupMiss NotFound {fr_pkg=Nothing,fr_mods_hidden=[],fr_pkgs_hidden=[],fr_unusables=[]} = True
isCleanModuleLookupMiss _ = False

publicModuleExports :: GhcMonad m => TcGblEnv -> String -> m (Maybe (Module,[Name]))
publicModuleExports checked requested
  | moduleName (tcg_mod checked) == mkModuleName requested =
      pure (Just (tcg_mod checked,concatMap availNames (tcg_exports checked)))
  | otherwise = do
      resolved <- handleSourceError (\failure -> do
          environment <- getSession
          found <- liftIO (findImportedModule environment (mkModuleName requested) NoPkgQual)
          if isCleanModuleLookupMiss found then pure Nothing else liftIO (throwIO failure))
        (Just <$> findModule (mkModuleName requested) Nothing)
      case resolved of
        Nothing -> pure Nothing
        Just owner -> do
          information <- getModuleInfo owner >>= maybe
            (liftIO (ioError (userError ("inspection module information unavailable for " ++ requested)))) pure
          pure (Just (owner,modInfoExports information))

matchesNameQuery :: String -> Name -> Bool
matchesNameQuery query name = query == occurrence
  || maybe False (\owner -> query == owner ++ "." ++ occurrence) definingModule
  where
    occurrence = occNameString (nameOccName name)
    definingModule = moduleNameString . moduleName <$> nameModule_maybe name

namespaceMatches :: StructuredNameNamespace -> Name -> Bool
namespaceMatches namespace name = case namespace of
  StructuredAnyName -> True
  StructuredValueName -> isVarName name
  StructuredTypeName -> isTyConName name
  StructuredConstructorName -> isDataConName name
  StructuredFieldName -> isJust (fieldOcc_maybe (nameOccName name)) || isVarName name

inspectResolved :: StructuredMode -> StructuredInspection -> [Name] -> (Name, TyThing) -> InspectionResult
inspectResolved mode query visibleNames (name, thing) = case mode of
  StructuredInfo -> case identifierInfo query visibleNames name thing of
    Left detail -> InspectionStructuredError (StructuredUnsupported detail)
    Right details -> InspectionStructuredInfo details
  StructuredType -> case typeForThing thing of
    Nothing -> InspectionStructuredError (StructuredUnsupported (unsupportedThing thing))
    Just ty -> InspectionStructuredType TypeInfo
      { typeIdentifier = identifierRef name thing,
        typeExpression = describeType ty,
        typeProvenance = queryProvenance query
      }

identifierInfo :: StructuredInspection -> [Name] -> Name -> TyThing -> Either String IdentifierInfo
identifierInfo query visible name thing = do
  declaration <- declarationInfo visible thing
  pure IdentifierInfo
    { inspectedIdentifier = identifierRef name thing,
      identifierDeclaration = declaration,
      identifierParent = identifierRefForThing <$> tyThingParent_maybe thing,
      identifierProvenance = queryProvenance query
    }

declarationInfo :: [Name] -> TyThing -> Either String DeclarationInfo
declarationInfo visible thing = case thing of
  AnId identifier
    | isRecordSelector identifier -> case tyThingParent_maybe thing of
        Just parent -> Right (RecordSelectorDeclaration (identifierRefForThing parent) (describeType (idType identifier)))
        Nothing -> Left "record selector has no parent declaration"
    | otherwise -> Right (ValueDeclaration (describeType (idType identifier)))
  AConLike (RealDataCon constructor) -> case tyThingParent_maybe thing of
    Just parent -> Right (ConstructorDeclaration (identifierRefForThing parent) (describeConstructor visible constructor))
    Nothing -> Left "data constructor has no parent declaration"
  AConLike (PatSynCon _) -> Left "pattern synonym inspection is not supported"
  ATyCon tyCon
    | isClassTyCon tyCon -> case tyConClass_maybe tyCon of
        Nothing -> Left "class type constructor has no Class"
        Just cls -> Right (ClassDeclaration
          (map (render . varName) (classTyVars cls))
          (map describeType (classSCTheta cls))
          (map describeMethod (filter ((`elem` visible) . getName) (classMethods cls))))
    | Just rhs <- synTyConRhs_maybe tyCon -> Right (TypeSynonymDeclaration
        (map (render . varName) (tyConTyVars tyCon)) (describeType rhs))
    | isAlgTyCon tyCon ->
        let constructors = map (describeConstructor visible) (filter ((`elem` visible) . getName) (tyConDataCons tyCon))
            parameters = map (render . varName) (tyConTyVars tyCon)
         in if isNewTyCon tyCon
              then case constructors of
                [constructor] -> Right (NewtypeDeclaration parameters constructor)
                [] -> Left "abstract newtype constructor is not publicly visible"
                _ -> Left "newtype exposes more than one constructor"
              else Right (DataDeclaration parameters constructors)
    | otherwise -> Left "type family or unsupported type constructor"
  ACoAxiom _ -> Left "coercion axiom inspection is not supported"

describeConstructor :: [Name] -> DataCon -> ConstructorInfo
describeConstructor visible constructor = ConstructorInfo
  { constructorRef = identifierRef (getName constructor) (AConLike (RealDataCon constructor)),
    constructorType = describeType (dataConDisplayType False constructor),
    constructorArguments = map (describeType . scaledThing) (dataConOrigArgTys constructor),
    recordFields = map (\field -> FieldInfo (render (flLabel field))
      (describeType (dataConFieldType constructor (flLabel field))))
      (filter (\field -> flSelector field `elem` visible) (dataConFieldLabels constructor))
  }

describeMethod :: Id -> ClassMethodInfo
describeMethod method = ClassMethodInfo (identifierRef (getName method) (AnId method)) (describeType (idType method))

typeForThing :: TyThing -> Maybe Type
typeForThing thing = case thing of
  AnId identifier -> Just (idType identifier)
  AConLike (RealDataCon constructor) -> Just (dataConDisplayType False constructor)
  AConLike (PatSynCon _) -> Nothing
  ATyCon tyCon -> Just (tyConKind tyCon)
  ACoAxiom _ -> Nothing

describeType :: Type -> TypeExpression
describeType original =
  TypeExpression
    (renderUser ty)
    (map (renderUser . varName) variables)
    (map renderUser constraints)
  where
    ty = tidyInspectionType original
    (variables, constraints, _) = tcSplitSigmaTy ty

identifierRefForThing :: TyThing -> IdentifierRef
identifierRefForThing thing = identifierRef (getName thing) thing

identifierRef :: Name -> TyThing -> IdentifierRef
identifierRef name thing = IdentifierRef
  { identifierModule = maybe "" (moduleNameString . moduleName) (nameModule_maybe name),
    identifierName = occNameString (nameOccName name),
    identifierNamespace = case thing of
      AnId identifier | isRecordSelector identifier -> FieldIdentifier
      AnId _ -> ValueIdentifier
      AConLike _ -> ConstructorIdentifier
      ATyCon _ | isDataConName name -> ConstructorIdentifier
      ATyCon _ -> TypeIdentifier
      ACoAxiom _ -> TypeIdentifier
  }

-- | The canonical identity of a resolved name, independent of how the caller
-- happened to spell it. Enumeration and type search use this for the entry's
-- own identity; the plural reference lists remain dependencies.
nominalIdentity :: Name -> IdentifierNamespace -> IdentifierRef
nominalIdentity name namespace = IdentifierRef
  { identifierModule = maybe "" (moduleNameString . moduleName) (nameModule_maybe name),
    identifierName = occNameString (nameOccName name),
    identifierNamespace = namespace
  }

-- | Immediate nominal references, obtained from compiler declarations. Member
-- visibility matches the declaration renderer; following a reference never
-- recursively traverses the referenced declaration.
declarationReferences :: [Name] -> TyThing -> [IdentifierRef]
declarationReferences visible thing = orderedReferences $
  maybe [] (pure . identifierRefForThing) (tyThingParent_maybe thing)
    ++ case thing of
      AnId identifier -> typeReferences (idType identifier)
      AConLike (RealDataCon constructor) -> constructorReferences constructor
      AConLike (PatSynCon _) -> []
      ATyCon tyCon
        | Just cls <- tyConClass_maybe tyCon ->
            concatMap typeReferences (classSCTheta cls)
              ++ concatMap (\method -> identifierRefForThing (AnId method)
                  : typeReferences (idType method))
                (filter ((`elem` visible) . getName) (classMethods cls))
        | Just rhs <- synTyConRhs_maybe tyCon -> typeReferences rhs
        | isAlgTyCon tyCon -> concatMap constructorReferences
            (filter ((`elem` visible) . getName) (tyConDataCons tyCon))
        | otherwise -> typeReferences (tyConKind tyCon)
      ACoAxiom _ -> []
  where
    constructorReferences constructor =
      identifierRefForThing (AConLike (RealDataCon constructor))
        : typeReferences (dataConDisplayType False constructor)
          ++ [ IdentifierRef
                (maybe "" (moduleNameString . moduleName) (nameModule_maybe name))
                (occNameString (nameOccName name)) FieldIdentifier
             | field <- dataConFieldLabels constructor,
               let name = flSelector field,
               name `elem` visible
             ]

-- tyConsOfType includes nominal types underneath applications and constraints.
-- Sort by stable identity rather than GHC's allocation-dependent Unique order.
typeReferences :: Type -> [IdentifierRef]
typeReferences = orderedReferences . map (identifierRefForThing . ATyCon)
  . nonDetEltsUniqSet . tyConsOfType

orderedReferences :: [IdentifierRef] -> [IdentifierRef]
orderedReferences = nub . sortOn (\reference ->
  (identifierModule reference, identifierName reference, identifierNamespace reference))

queryProvenance :: StructuredInspection -> ScopeProvenance
queryProvenance query = ScopeProvenance
  { provenanceScope = structuredScope query,
    provenanceGeneration = inspectionGeneration (structuredProvenance query),
    provenanceFingerprint = inspectionFingerprint (structuredProvenance query)
  }

unsupportedThing :: TyThing -> String
unsupportedThing thing = "unsupported declaration: " ++ thingKind thing

inspectName :: (GhcMonad m) => AvailabilityContext -> GlobalRdrEnv -> String -> m InspectionResult
inspectName context rdrEnv query = do
  let names =
        [ greName gre
        | gre <- globalRdrEnvElts rdrEnv,
          any (matchesQuery query) (greRdrNames gre)
        ]
  -- Everything the query module can name. A constructor hidden behind an
  -- abstract export is not in this environment, which is exactly the set the
  -- rendering must respect.
  let inScope = nub (map greName (globalRdrEnvElts rdrEnv))
  entries <- entriesFor context inScope (nubBy (==) names)
  let preferred =
        if any ((== "type") . infoKind) entries
          then filter ((/= "constructor") . infoKind) entries
          else entries
  pure $ case preferred of
    [] -> InspectionNotFound query
    [_] -> InspectionInfo query preferred
    _ -> InspectionAmbiguous query preferred

exactThingMatches :: StructuredNameNamespace -> TyThing -> Bool
exactThingMatches StructuredFieldName (AnId identifier) = isRecordSelector identifier
exactThingMatches StructuredFieldName _ = False
exactThingMatches _ _ = True

hasExportedParent :: [Name] -> TyThing -> Bool
hasExportedParent exported thing = case tyThingParent_maybe thing of
  Just parent -> getName parent `elem` exported
  Nothing -> False

visibleInspectionName :: Name -> Bool
visibleInspectionName = not . isPrefixOf "__tidepool_" . occNameString . nameOccName

-- | Apply an extraction inspection budget. `Nothing` means the caller asked for
-- the complete enumeration. The budget counts names, and browsing renders one
-- entry per surviving name, so it also bounds the encoded entry list.
boundedNames :: Maybe Int -> [a] -> [a]
boundedNames Nothing = id
boundedNames (Just budget) = take budget

-- | Deduplicate before budgeting. An export list may name the same entity
-- twice; taking the budget first would retain fewer than `budget` distinct
-- entries and make a truncated answer look shorter than it is.
boundedDistinctNames :: (Ord a) => Maybe Int -> [a] -> [a]
boundedDistinctNames budget = boundedNames budget . distinctOn id

distinctOn :: Ord key => (value -> key) -> [value] -> [value]
distinctOn key = go Set.empty
  where
    go _ [] = []
    go seen (value:rest)
      | key value `Set.member` seen = go seen rest
      | otherwise = value : go (Set.insert (key value) seen) rest

inspectModule :: (GhcMonad m) => AvailabilityContext -> String -> Bool -> Maybe Int -> m InspectionResult
inspectModule context requested expanded budget
  | isReservedSessionModuleName requested = pure (InspectionModuleNotFound requested [])
  | otherwise = do
    resolved <- publicModuleExports (availabilityTcGblEnv context) requested
    case resolved of
      Nothing -> pure (InspectionModuleNotFound requested [StructuredPublicModule requested])
      Just (mdl,names) -> browse mdl names
  where
    browse mdl names = do
      -- Deduplicate identities before the budget, then collapse distinct
      -- entities that have identical public rendering after presentation.
      rawEntries <- browseEntries context expanded (boundedDistinctNames budget names)
      let entries = nubBy sameDisplay (sortOn entryKey rawEntries)
      pure (InspectionBrowse (moduleNameString (moduleName mdl)) expanded entries)
    sameDisplay left right = infoDisplay left == infoDisplay right
    entryKey entry = (infoName entry, infoKind entry, infoDisplay entry)

-- | Render a declaration showing only the sub-components the asker can name.
--
-- `pprTyThingInContext` overwrites `ss_how_much` with an unfiltered
-- `ShowSome Nothing` whenever the thing has no parent, so an abstractly
-- exported type printed every hidden constructor as though a cell could write
-- one. A live agent did exactly that, was told `Data constructor not in scope`,
-- and spent four discovery rounds recovering. With a real predicate GHC's
-- `ppr_trim` collapses the hidden run to `...`, which is what `:browse` shows.
--
-- A thing that does have a parent keeps the context rendering unchanged: there
-- the filter is the parent chain, which is already the right answer.
visibleDisplay :: [Name] -> TyThing -> String
visibleDisplay visible thing =
  renderWithContext defaultSDocContext $ case tyThingParent_maybe thing of
    Just _ -> pprTyThingInContext showEverything thing
    Nothing -> pprTyThing showOnlyVisible thing
  where
    visibleOccs = map nameOccName visible
    showOnlyVisible =
      ShowSub
        { ss_how_much = ShowSome (Just (`elem` visibleOccs)) (AltPpr Nothing),
          ss_forall = ShowForAllWhen
        }

data EntryPresentation = ContextualEntry | ExpandedEntry

entryDisplay :: EntryPresentation -> [Name] -> TyThing -> String
entryDisplay ExpandedEntry _ thing | isJust (tyThingParent_maybe thing) =
  renderWithContext defaultSDocContext (pprTyThing showEverything thing)
entryDisplay _ visible thing = visibleDisplay visible thing

-- | The query module is typechecked, never loaded, so 'getInfo' cannot see its
-- own top-level binders: the preamble's declarations, including a pending
-- request's @respond@/@sessionReply@/@sessionInput@. Those resolve through the
-- typechecked module's own type environment instead, and report no defining
-- module, because the generated query module is not one a cell can import.
entriesFor :: (GhcMonad m) => AvailabilityContext -> [Name] -> [Name] -> m [InfoEntry]
entriesFor context = entriesForMatching context ContextualEntry (const True)

entriesForMatching :: (GhcMonad m) => AvailabilityContext -> EntryPresentation -> (TyThing -> Bool) -> [Name] -> [Name] -> m [InfoEntry]
entriesForMatching context presentation matches visible names = fmap concat $ forM names $ \name -> do
  resolveNameWork context
  found <- getInfo False name
  let tcg = availabilityTcGblEnv context
      local
        | nameIsLocalOrFrom (tcg_mod tcg) name = lookupTypeEnv (tcg_type_env tcg) name
        | otherwise = Nothing
      resolved = case found of
        Just (thing, _fixity, _instances, _families, _extra) -> Just (thing, False)
        Nothing -> (\thing -> (thing, True)) <$> local
  case resolved of
    Nothing -> pure []
    Just (thing, _) | not (matches thing) -> pure []
    Just (thing, isLocal) ->
      let display = entryDisplay presentation visible thing
          definingModule
            | isLocal = Nothing
            | otherwise = moduleNameString . moduleName <$> nameModule_maybe name
       in do
          richEntryWork context
          availability <- thingAvailability context thing
          pure
            [ InfoEntry
                { infoName = occNameString (nameOccName name),
                  infoNominalName = name,
                  infoNamespace = identifierNamespace (identifierRef name thing),
                  infoModule = definingModule,
                  infoKind = thingKind thing,
                  infoDisplay = display,
                  infoAvailability = availability,
                  infoReferences = declarationReferences visible thing
                }
            ]

browseEntries :: (GhcMonad m) => AvailabilityContext -> Bool -> [Name] -> m [InfoEntry]
browseEntries context expanded names = do
  found <- forM names $ \name -> do
    resolveNameWork context
    let checked = availabilityTcGblEnv context
        local
          | nameIsLocalOrFrom (tcg_mod checked) name = lookupTypeEnv (tcg_type_env checked) name
          | otherwise = Nothing
    thing <- requireLookupName name $ case local of
      Just value -> pure (Just value)
      Nothing -> lookupName name
    pure (name,thing)
  let exportedNames = map fst found
      visible =
        if expanded
          then found
          else filter (not . hasExportedParent exportedNames . snd) found
  forM visible $ \(name, thing) -> do
    richEntryWork context
    availability <- thingAvailability context thing
    let display = entryDisplay (if expanded then ExpandedEntry else ContextualEntry) exportedNames thing
        definingModule = moduleNameString . moduleName <$> nameModule_maybe name
    pure InfoEntry
          { infoName = occNameString (nameOccName name),
            infoNominalName = name,
            infoNamespace = identifierNamespace (identifierRef name thing),
            infoModule = definingModule,
            infoKind = thingKind thing,
            infoDisplay = display,
            infoAvailability = availability,
            infoReferences = declarationReferences exportedNames thing
          }

thingAvailability :: (GhcMonad m) => AvailabilityContext -> TyThing -> m Availability
thingAvailability context thing = case thing of
  AnId identifier -> liftIO $ signatureAvailability context (idType identifier)
  AConLike constructor -> pure $ if isVanillaConLike constructor then Available else Unknown
  _ -> pure Available

-- Dots may belong to an operator occurrence; compare the actual reader
-- spelling instead of splitting the caller text into guessed name parts.
matchesQuery :: String -> RdrName -> Bool
matchesQuery query reader = case reader of
  Unqual occurrence -> query == occNameString occurrence
  Qual alias occurrence -> query == moduleNameString alias ++ "." ++ occNameString occurrence
  Orig _ _ -> False
  Exact _ -> False

showEverything :: ShowSub
showEverything = ShowSub ShowIface ShowForAllWhen

thingKind :: TyThing -> String
thingKind thing = case thing of
  AnId identifier
    | isJust (isClassOpId_maybe identifier) -> "class-method"
    | isRecordSelector identifier -> "record-selector"
    | otherwise -> "value"
  AConLike _ -> "constructor"
  ATyCon _ -> "type"
  ACoAxiom _ -> "coercion"

render :: Outputable value => value -> String
render = renderWithContext defaultSDocContext . ppr

-- Type variables may retain compiler allocation suffixes in the typechecked
-- source. They are semantically irrelevant and vary when otherwise independent
-- inspection probes share a module, so remove them before producing any
-- user-facing type text.
renderType :: Type -> String
renderType = renderUser . tidyInspectionType

renderUser :: Outputable value => value -> String
renderUser = renderWithContext defaultSDocContext {sdocSuppressUniques = True} . ppr

tidyInspectionType :: Type -> Type
tidyInspectionType = tidyOpenType emptyTidyEnv

-- | Matched inspection output for complete requests.
encodeInspectionResults :: [InspectionResult] -> BS.ByteString
encodeInspectionResults results = inspectionOutput
  (encodeListLen 2 <> encodeString "Complete" <> encodeList encodeResult results)

inspectionOutput :: Encoding -> BS.ByteString
inspectionOutput body = toStrictByteString (encodeListLen 2 <> encodeString "TPINSP008" <> body)

encodeResult :: InspectionResult -> Encoding
encodeResult inspection = case inspection of
  InspectionRowFit ref availability ->
    encodeListLen 3 <> encodeString "RowFit" <> encodeIdentifierRef ref <> encodeAvailability availability
  InspectionType expression display availability ->
    encodeListLen 4 <> encodeString "Type" <> encodeText expression <> encodeText display
      <> encodeAvailability availability
  InspectionInfo query entries ->
    encodeListLen 3 <> encodeString "Info" <> encodeText query <> encodeEntries entries
  InspectionAmbiguous query entries ->
    encodeListLen 3 <> encodeString "Ambiguous" <> encodeText query <> encodeEntries entries
  InspectionNotFound query ->
    encodeListLen 2 <> encodeString "NotFound" <> encodeText query
  InspectionModuleNotFound moduleName scopes ->
    encodeListLen 3 <> encodeString "ModuleNotFound" <> encodeText moduleName <> encodeList encodeScope scopes
  InspectionRejected diagnostic ->
    encodeListLen 2 <> encodeString "Rejected" <> encodeText diagnostic
  InspectionBrowse moduleName expanded entries ->
    encodeListLen 4
      <> encodeString "Browse"
      <> encodeText moduleName
      <> encodeBool expanded
      <> encodeEntries entries
  InspectionTypeMatches query matches ->
    encodeListLen 3
      <> encodeString "TypeMatches"
      <> encodeText query
      <> encodeTypeMatches matches
  InspectionStructuredInfo details ->
    encodeListLen 2 <> encodeString "StructuredInfoOk" <> encodeIdentifierInfo details
  InspectionStructuredType details ->
    encodeListLen 2 <> encodeString "StructuredTypeOk" <> encodeTypeInfo details
  InspectionStructuredError failure ->
    encodeListLen 2 <> encodeString "StructuredError" <> encodeStructuredError failure
encodeEntries :: [InfoEntry] -> Encoding
encodeEntries entries =
  encodeListLen (fromIntegral (length entries)) <> foldMap encodeEntry entries
encodeEntry :: InfoEntry -> Encoding
encodeEntry entry =
  encodeListLen 7
    <> encodeText (infoName entry)
    <> maybe encodeNull encodeText (infoModule entry)
    <> encodeText (infoKind entry)
    <> encodeText (infoDisplay entry)
    <> encodeAvailability (infoAvailability entry)
    <> encodeList encodeIdentifierRef (infoReferences entry)
    <> encodeIdentifierRef (nominalIdentity (infoNominalName entry) (infoNamespace entry))
encodeTypeMatches :: [TypeMatch] -> Encoding
encodeTypeMatches matches =
  encodeListLen (fromIntegral (length matches)) <> foldMap encodeTypeMatch matches
encodeTypeMatch :: TypeMatch -> Encoding
encodeTypeMatch match =
  encodeListLen 7
    <> encodeText (typeMatchName match)
    <> maybe encodeNull encodeText (typeMatchModule match)
    <> encodeText (typeMatchSignature match)
    <> encodeString (case typeMatchQuality match of
      TypeMatchExact -> "Exact"
      TypeMatchUsable -> "Usable")
    <> encodeAvailability (typeMatchAvailability match)
    <> encodeList encodeIdentifierRef (typeMatchReferences match)
    <> encodeIdentifierRef
      (nominalIdentity (typeMatchNominalName match) (typeMatchNamespace match))
encodeAvailability :: Availability -> Encoding
encodeAvailability = encodeString . T.pack . show

encodeStructuredError :: StructuredQueryError -> Encoding
encodeStructuredError failure = case failure of
  StructuredUnknown query -> encodeListLen 2 <> encodeString "Unknown" <> encodeStructuredQuery query
  StructuredAmbiguous query candidates -> encodeListLen 3 <> encodeString "Ambiguous"
    <> encodeStructuredQuery query <> encodeList encodeIdentifierRef candidates
  StructuredUnknownModule moduleName -> encodeListLen 2 <> encodeString "UnknownModule" <> encodeText moduleName
  StructuredUnsupported detail -> encodeListLen 2 <> encodeString "Unsupported" <> encodeText detail

encodeIdentifierInfo :: IdentifierInfo -> Encoding
encodeIdentifierInfo details = encodeListLen 4
  <> encodeIdentifierRef (inspectedIdentifier details)
  <> encodeDeclaration (identifierDeclaration details)
  <> maybe encodeNull encodeIdentifierRef (identifierParent details)
  <> encodeProvenance (identifierProvenance details)

encodeTypeInfo :: TypeInfo -> Encoding
encodeTypeInfo details = encodeListLen 3
  <> encodeIdentifierRef (typeIdentifier details)
  <> encodeTypeExpression (typeExpression details)
  <> encodeProvenance (typeProvenance details)

encodeDeclaration :: DeclarationInfo -> Encoding
encodeDeclaration declaration = case declaration of
  ValueDeclaration ty -> encodeListLen 2 <> encodeString "Value" <> encodeTypeExpression ty
  DataDeclaration parameters constructors -> encodeListLen 3 <> encodeString "Data"
    <> encodeTexts parameters <> encodeList encodeConstructor constructors
  NewtypeDeclaration parameters constructor -> encodeListLen 3 <> encodeString "Newtype"
    <> encodeTexts parameters <> encodeConstructor constructor
  TypeSynonymDeclaration parameters rhs -> encodeListLen 3 <> encodeString "TypeSynonym"
    <> encodeTexts parameters <> encodeTypeExpression rhs
  ClassDeclaration parameters supers methods -> encodeListLen 4 <> encodeString "Class"
    <> encodeTexts parameters <> encodeList encodeTypeExpression supers <> encodeList encodeClassMethod methods
  ConstructorDeclaration parent constructor -> encodeListLen 3 <> encodeString "Constructor"
    <> encodeIdentifierRef parent <> encodeConstructor constructor
  RecordSelectorDeclaration parent ty -> encodeListLen 3 <> encodeString "RecordSelector"
    <> encodeIdentifierRef parent <> encodeTypeExpression ty

encodeConstructor :: ConstructorInfo -> Encoding
encodeConstructor constructor = encodeListLen 4
  <> encodeIdentifierRef (constructorRef constructor)
  <> encodeTypeExpression (constructorType constructor)
  <> encodeList encodeTypeExpression (constructorArguments constructor)
  <> encodeList encodeField (recordFields constructor)

encodeField :: FieldInfo -> Encoding
encodeField field = encodeListLen 2 <> encodeText (fieldName field) <> encodeTypeExpression (fieldType field)

encodeClassMethod :: ClassMethodInfo -> Encoding
encodeClassMethod method = encodeListLen 2 <> encodeIdentifierRef (classMethodRef method)
  <> encodeTypeExpression (classMethodType method)

encodeTypeExpression :: TypeExpression -> Encoding
encodeTypeExpression ty = encodeListLen 3 <> encodeText (typeCanonical ty)
  <> encodeTexts (typeVariables ty) <> encodeTexts (typeConstraints ty)

encodeProvenance :: ScopeProvenance -> Encoding
encodeProvenance provenance = encodeListLen 3 <> encodeScope (provenanceScope provenance)
  <> encodeWord64 (provenanceGeneration provenance) <> encodeText (provenanceFingerprint provenance)

encodeStructuredQuery :: StructuredInspection -> Encoding
encodeStructuredQuery query = encodeListLen 3 <> encodeScope (structuredScope query)
  <> encodeString (case structuredNamespace query of
    StructuredAnyName -> "Any"
    StructuredValueName -> "Value"
    StructuredTypeName -> "Type"
    StructuredConstructorName -> "Constructor"
    StructuredFieldName -> "Field")
  <> encodeText (structuredName query)

encodeScope :: StructuredNameScope -> Encoding
encodeScope scope = case scope of
  StructuredCurrentScope -> encodeListLen 1 <> encodeString "Current"
  StructuredPublicModule moduleName -> encodeListLen 2 <> encodeString "PublicModule" <> encodeText moduleName

encodeIdentifierRef :: IdentifierRef -> Encoding
encodeIdentifierRef identifier = encodeListLen 3 <> encodeText (identifierModule identifier)
  <> encodeText (identifierName identifier) <> encodeString (case identifierNamespace identifier of
    ValueIdentifier -> "Value"
    TypeIdentifier -> "Type"
    ConstructorIdentifier -> "Constructor"
    FieldIdentifier -> "Field")

encodeTexts :: [String] -> Encoding
encodeTexts = encodeList encodeText

encodeList :: (value -> Encoding) -> [value] -> Encoding
encodeList encode values = encodeListLen (fromIntegral (length values)) <> foldMap encode values

encodeText :: String -> Encoding
encodeText = encodeString . T.pack
