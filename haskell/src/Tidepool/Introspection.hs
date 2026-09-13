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
    normalizeLookupWildcards,
    searchTypeMatches,
    runInspection,
    encodeInspectionResults,
  )
where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Control.Monad (foldM, forM)
import Control.Monad.IO.Class (liftIO)
import Control.Monad.State.Strict (State, evalState, get, put)
import Data.ByteString qualified as BS
import Data.Generics (everything, everywhereM, mkM, mkQ)
import Data.List (nub, nubBy, sortOn)
import Data.Map.Strict qualified as Map
import Data.Maybe (catMaybes, isJust, listToMaybe)
import GHC.Core.TyCo.FVs (tyCoVarsOfTypes)
import GHC.Core.TyCo.Subst (emptySubst, extendTvSubst)
import GHC.Core.Type (getTyVar_maybe, splitTyConApp_maybe, substTy)
import GHC.Tc.Solver (tcCheckWanteds)
import GHC.Tc.Solver.InertSet (emptyInert)
import GHC.Tc.Types (TcGblEnv)
import GHC.Tc.Utils.Monad (initTcWithGbl)
import GHC.Types.SrcLoc (mkRealSrcSpan, mkRealSrcLoc)
import GHC.Data.FastString (mkFastString)
import GHC.Types.Var.Set (isEmptyVarSet)
import Data.Set qualified as Set
import Data.Text qualified as T
import Data.Word (Word64)
import GHC
import GHC.Core.Class (classTyVars)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.ConLike (ConLike (..), isVanillaConLike)
import GHC.Core.DataCon (dataConDisplayType, dataConFieldType, dataConOrigArgTys)
import GHC.Core.Multiplicity (scaledThing)
import GHC.Core.TyCon (isAlgTyCon)
import GHC.Core.Unify (tcMatchTy)
import GHC.Iface.Type (ShowForAllFlag (..), ShowHowMuch (..), ShowSub (..))
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name (isDataConName, isTyConName, isVarName)
import GHC.Types.Name.Occurrence (isSymOcc, mkTyVarOcc, occNameString)
import GHC.Types.Name.Reader (GlobalRdrEnv, RdrName (..), globalRdrEnvElts, greName, greRdrNames, mkRdrUnqual, rdrNameOcc)
import GHC.Types.TyThing (tyThingParent_maybe)
import GHC.Types.TyThing.Ppr (pprTyThing, pprTyThingInContext)
import GHC.Types.FieldLabel (flLabel, flSelector)
import GHC.Types.Var (varName)
import GHC.Tc.Utils.TcType (tcSplitFunTys, tcSplitSigmaTy)
import GHC.Utils.Outputable (Outputable, defaultSDocContext, ppr, renderWithContext)
import Tidepool.ExtractRequest
  ( InspectionProvenance (..), InspectionRequest (..), StructuredInspection (..),
    StructuredNameNamespace (..), StructuredNameScope (..)
  )
import Tidepool.ExtractUtil (getLibdir)

data InfoEntry = InfoEntry
  { infoName :: String,
    infoModule :: Maybe String,
    infoKind :: String,
    infoDisplay :: String,
    infoAvailability :: Availability
  }
  deriving (Eq, Show)

data InspectionResult
  = InspectionType String String Availability
  | InspectionInfo String [InfoEntry]
  | InspectionAmbiguous String [InfoEntry]
  | InspectionNotFound String
  | InspectionModuleNotFound String
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

data Availability = Available | Unknown | Unavailable
  deriving (Eq, Ord, Show)

data TypeMatch = TypeMatch
  { typeMatchName :: String,
    typeMatchModule :: Maybe String,
    typeMatchSignature :: String,
    typeMatchQuality :: TypeMatchQuality,
    typeMatchAvailability :: Availability
  }
  deriving (Eq, Show)

data IdentifierNamespace
  = ValueIdentifier
  | TypeIdentifier
  | ConstructorIdentifier
  | FieldIdentifier
  deriving (Eq, Show)

data IdentifierRef = IdentifierRef
  { identifierModule :: String,
    identifierName :: String,
    identifierNamespace :: IdentifierNamespace
  }
  deriving (Eq, Show)

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
    availabilityRow :: Maybe Type
  }

-- | The sentinel is generated with a qualified Data.Proxy signature. Its
-- argument is inspected as a GHC Type; rendered text never determines a row.
lookupExecutionRow :: (GhcMonad m) => GlobalRdrEnv -> m (Maybe Type)
lookupExecutionRow rdrEnv = do
  let names =
        [ greName gre
        | gre <- globalRdrEnvElts rdrEnv,
          any (matchesQuery "__tidepool_lookup_row") (greRdrNames gre)
        ]
  case nubBy (==) names of
    [name] -> do
      found <- lookupName name
      proxy <- lookupProxyTyCon
      case (found, proxy) of
        (Just (AnId identifier), Just proxyConstructor) ->
          case splitTyConApp_maybe (idType identifier) of
            Just (constructor, arguments)
              | constructor == proxyConstructor,
                Just row <- listToMaybe (reverse arguments) -> pure (Just row)
            _ -> malformed
        _ -> malformed
    _ -> pure Nothing
  where
    malformed = liftIO (ioError (userError "inspection row sentinel has no typed Proxy row"))

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
  pure $ if rowDecision == Unavailable || not closedSolved
    then Unavailable
    else if allSolved && rowDecision == Available then Available else Unknown

solvePredicates :: AvailabilityContext -> [Type] -> IO Bool
solvePredicates context predicates =
  if null predicates
    then pure True
    else do
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
normalizeLookupWildcards :: ParsedModule -> ParsedModule
normalizeLookupWildcards parsed =
  let source = pm_parsed_source parsed
      occupied =
        Set.fromList $
          everything (++) (mkQ [] (\name -> [occNameString (rdrNameOcc name)])) source
   in parsed
        { pm_parsed_source =
            evalState (everywhereM (mkM replaceWildcard) source) (0, occupied)
        }
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
      let candidate = "__lookup_w" ++ show index
       in if Set.member candidate occupied
            then freshName occupied (index + 1)
            else (index + 1, candidate)

-- | Match a checked lookup type against every value in the exact reader
-- environment of the inspection module. Matching is entirely in memory:
-- callers compile the query once, never once per candidate.
searchTypeMatches :: (GhcMonad m) => HscEnv -> TcGblEnv -> GlobalRdrEnv -> Name -> Type -> m [TypeMatch]
searchTypeMatches hscEnv tcGblEnv rdrEnv queryBinder query = do
  row <- lookupExecutionRow rdrEnv
  effTyCon <- lookupEffTyCon
  case (row, effTyCon) of
    (Just _, Nothing) -> liftIO (ioError (userError "inspection could not resolve Eff type constructor"))
    _ -> pure ()
  searchTypeMatchesWithContext (AvailabilityContext hscEnv tcGblEnv effTyCon row) rdrEnv queryBinder query

searchTypeMatchesWithContext :: (GhcMonad m) => AvailabilityContext -> GlobalRdrEnv -> Name -> Type -> m [TypeMatch]
searchTypeMatchesWithContext context rdrEnv queryBinder query = do
  let entries =
        filter ((/= queryBinder) . greName) (globalRdrEnvElts rdrEnv)
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
    found <- lookupName name
    pure $ case found of
      Just (AnId identifier) -> Just (name, spelling, idType identifier)
      _ -> Nothing
  matches <- forM things $ \(name, spelling, candidate) ->
    case matchQuality query candidate of
      Nothing -> pure Nothing
      Just (quality, substitution) -> do
        let (_, predicates, body) = tcSplitSigmaTy candidate
            specialized = maybe id substTy substitution
        availability <- liftIO $ availabilityFor context
          (Just (specialized body)) (map specialized predicates)
        pure (Just (toMatch name spelling candidate quality availability))
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

    toMatch name spelling candidate quality availability =
      TypeMatch
        { typeMatchName = spelling,
          typeMatchModule = moduleNameString . moduleName <$> nameModule_maybe name,
          typeMatchSignature =
            renderWithContext defaultSDocContext (ppr candidate),
          typeMatchQuality = quality,
          typeMatchAvailability = availability
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
  Map.Map String String ->
  [InspectionRequest] ->
  IO [InspectionResult]
runInspection hscEnv tcGblEnv rdrEnv capturedTypes requests = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    setSession hscEnv
    effTyCon <- lookupEffTyCon
    row <- lookupExecutionRow rdrEnv
    case (row, effTyCon) of
      (Just _, Nothing) -> liftIO (ioError (userError "inspection could not resolve Eff type constructor"))
      _ -> pure ()
    let context = AvailabilityContext hscEnv tcGblEnv effTyCon row
    snd <$> foldM (inspect context) (0 :: Int, []) requests
  where
    inspect context (typeIndex, results) request = case request of
      InspectTypeOf expression ->
        let binder = "__tidepool_inspect_" ++ show typeIndex
         in case Map.lookup binder capturedTypes of
              Just display -> do
                let names =
                      [ greName gre
                      | gre <- globalRdrEnvElts rdrEnv,
                        any (matchesQuery binder) (greRdrNames gre)
                      ]
                case nubBy (==) names of
                  [name] -> do
                    found <- lookupName name
                    case found of
                      Just (AnId identifier) -> do
                        availability <- liftIO $ signatureAvailability context (idType identifier)
                        pure (typeIndex + 1, results ++ [InspectionType expression display availability])
                      _ -> missing binder
                  _ -> missing binder
              Nothing -> liftIO (ioError (userError ("inspection module did not expose " ++ binder)))
      InspectNameInfo query -> do
        result <- inspectName context rdrEnv query
        pure (typeIndex, results ++ [result])
      InspectModule moduleName expanded -> do
        result <- inspectModule context moduleName expanded
        pure (typeIndex, results ++ [result])
      InspectTypeSearch query -> do
        result <- inspectTypeSearch context rdrEnv query
        pure (typeIndex, results ++ [result])
      InspectStructuredInfoOf query -> do
        result <- inspectStructured rdrEnv StructuredInfo query
        pure (typeIndex, results ++ [result])
      InspectStructuredTypeOf query -> do
        result <- inspectStructured rdrEnv StructuredType query
        pure (typeIndex, results ++ [result])
    missing binder = liftIO (ioError (userError ("inspection module did not expose " ++ binder)))

inspectTypeSearch :: (GhcMonad m) => AvailabilityContext -> GlobalRdrEnv -> String -> m InspectionResult
inspectTypeSearch context rdrEnv query = do
  let binderName = "__tidepool_lookup_query"
      binders =
        [ greName gre
        | gre <- globalRdrEnvElts rdrEnv,
          any (matchesQuery binderName) (greRdrNames gre)
        ]
  case nubBy (==) binders of
    [binder] -> do
      found <- lookupName binder
      case found of
        Just (AnId identifier) ->
          InspectionTypeMatches query <$> searchTypeMatchesWithContext context rdrEnv binder (idType identifier)
        _ -> missing binderName
    _ -> missing binderName
  where
    missing binder =
      liftIO (ioError (userError ("lookup module did not expose " ++ binder)))

data StructuredMode = StructuredInfo | StructuredType

inspectStructured :: GhcMonad m => GlobalRdrEnv -> StructuredMode -> StructuredInspection -> m InspectionResult
inspectStructured rdrEnv mode query = do
  candidates <- namesInScope rdrEnv query
  case candidates of
    Left missingModule -> pure (InspectionStructuredError (StructuredUnknownModule missingModule))
    Right (names, visibleNames) -> do
      things <- fmap catMaybes $ forM names $ \name -> do
        found <- lookupName name
        pure ((name,) <$> found)
      case things of
        [] -> pure (InspectionStructuredError (StructuredUnknown query))
        [resolved] -> pure (inspectResolved mode query visibleNames resolved)
        many -> pure (InspectionStructuredError
          (StructuredAmbiguous query (map (uncurry identifierRef) many)))

namesInScope :: GhcMonad m => GlobalRdrEnv -> StructuredInspection -> m (Either String ([Name], [Name]))
namesInScope rdrEnv query = case structuredScope query of
  StructuredCurrentScope -> pure (Right (filter matches currentNames, currentNames))
  StructuredPublicModule requested -> handleSourceError (\_ -> pure (Left requested)) $ do
    mdl <- findModule (mkModuleName requested) Nothing
    resolvedInfo <- getModuleInfo mdl
    let visible = nub (maybe [] modInfoExports resolvedInfo)
    pure (Right (filter matches visible, visible))
  where
    currentNames = nub (map greName (globalRdrEnvElts rdrEnv))
    matches name = matchesNameQuery (structuredName query) name
      && namespaceMatches (structuredNamespace query) name

matchesNameQuery :: String -> Name -> Bool
matchesNameQuery query name =
  occNameString (nameOccName name) == occurrence
    && maybe True (\wanted -> definingModule == Just wanted) qualifier
  where
    (qualifier, occurrence) = case break (== '.') (reverse query) of
      (reversedOccurrence, []) -> (Nothing, reverse reversedOccurrence)
      (reversedOccurrence, _ : reversedQualifier) ->
        (Just (reverse reversedQualifier), reverse reversedOccurrence)
    definingModule = moduleNameString . moduleName <$> nameModule_maybe name

namespaceMatches :: StructuredNameNamespace -> Name -> Bool
namespaceMatches namespace name = case namespace of
  StructuredAnyName -> True
  StructuredValueName -> isVarName name
  StructuredTypeName -> isTyConName name
  StructuredConstructorName -> isDataConName name

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
describeType ty = TypeExpression (render ty) (map (render . varName) variables) (map render constraints)
  where
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
      ATyCon _ -> TypeIdentifier
      ACoAxiom _ -> TypeIdentifier
  }

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
  entries <- entriesFor context (nubBy (==) names)
  let preferred =
        if any ((== "type") . infoKind) entries
          then filter ((/= "constructor") . infoKind) entries
          else entries
  pure $ case preferred of
    [] -> InspectionNotFound query
    [_] -> InspectionInfo query preferred
    _ -> InspectionAmbiguous query preferred

inspectModule :: (GhcMonad m) => AvailabilityContext -> String -> Bool -> m InspectionResult
inspectModule context requested expanded = handleSourceError
  (\_ -> pure (InspectionModuleNotFound requested))
  $ do
    mdl <- findModule (mkModuleName requested) Nothing
    resolvedInfo <- getModuleInfo mdl
    let names = maybe [] modInfoExports resolvedInfo
    rawEntries <- browseEntries context expanded names
    let entries = nubBy sameDisplay (sortOn entryKey rawEntries)
    pure (InspectionBrowse (moduleNameString (moduleName mdl)) expanded entries)
  where
    sameDisplay left right = infoDisplay left == infoDisplay right
    entryKey entry = (infoName entry, infoKind entry, infoDisplay entry)

entriesFor :: (GhcMonad m) => AvailabilityContext -> [Name] -> m [InfoEntry]
entriesFor context names = fmap concat $ forM names $ \name -> do
  found <- getInfo False name
  case found of
    Nothing -> pure []
    Just (thing, _fixity, _instances, _families, _extra) ->
      let display =
            renderWithContext
              defaultSDocContext
              (pprTyThingInContext showEverything thing)
          definingModule = moduleNameString . moduleName <$> nameModule_maybe name
       in do
          availability <- thingAvailability context thing
          pure
            [ InfoEntry
                { infoName = occNameString (nameOccName name),
                  infoModule = definingModule,
                  infoKind = thingKind thing,
                  infoDisplay = display,
                  infoAvailability = availability
                }
            ]

browseEntries :: (GhcMonad m) => AvailabilityContext -> Bool -> [Name] -> m [InfoEntry]
browseEntries context expanded names = do
  found <- fmap catMaybes $ forM names $ \name -> do
    thing <- lookupName name
    pure (fmap (\value -> (name, value)) thing)
  let exportedNames = map fst found
      visible =
        if expanded
          then found
          else filter (not . hasExportedParent exportedNames . snd) found
  forM visible $ \(name, thing) -> do
    availability <- thingAvailability context thing
    let document =
          if expanded
            then pprTyThing showEverything thing
            else pprTyThingInContext showEverything thing
        display = renderWithContext defaultSDocContext document
        definingModule = moduleNameString . moduleName <$> nameModule_maybe name
    pure InfoEntry
          { infoName = occNameString (nameOccName name),
            infoModule = definingModule,
            infoKind = thingKind thing,
            infoDisplay = display,
            infoAvailability = availability
          }
  where
    hasExportedParent exported thing = case tyThingParent_maybe thing of
      Just parent -> getName parent `elem` exported
      Nothing -> False

thingAvailability :: (GhcMonad m) => AvailabilityContext -> TyThing -> m Availability
thingAvailability context thing = case thing of
  AnId identifier -> liftIO $ signatureAvailability context (idType identifier)
  AConLike constructor -> pure $ if isVanillaConLike constructor then Available else Unknown
  _ -> pure Available

matchesQuery :: String -> RdrName -> Bool
matchesQuery query reader = case reader of
  Unqual occurrence -> qualifier == Nothing && occNameString occurrence == wanted
  Qual alias occurrence -> qualifier == Just (moduleNameString alias) && occNameString occurrence == wanted
  Orig _ _ -> False
  Exact _ -> False
  where
    (qualifier, wanted) = case break (== '.') (reverse query) of
      (reversedOccurrence, []) -> (Nothing, reverse reversedOccurrence)
      (reversedOccurrence, _ : reversedQualifier) ->
        (Just (reverse reversedQualifier), reverse reversedOccurrence)

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

-- | Private V5 batch receipt. The outer list is @['TPINSP005', results]@.
encodeInspectionResults :: [InspectionResult] -> BS.ByteString
encodeInspectionResults results =
  toStrictByteString $
    encodeListLen 2
      <> encodeString "TPINSP005"
      <> encodeListLen (fromIntegral (length results))
      <> foldMap encodeResult results
  where
    encodeResult inspection = case inspection of
      InspectionType expression display availability ->
        encodeListLen 4 <> encodeString "Type" <> text expression <> text display
          <> encodeAvailability availability
      InspectionInfo query entries ->
        encodeListLen 3 <> encodeString "Info" <> text query <> encodeEntries entries
      InspectionAmbiguous query entries ->
        encodeListLen 3 <> encodeString "Ambiguous" <> text query <> encodeEntries entries
      InspectionNotFound query ->
        encodeListLen 2 <> encodeString "NotFound" <> text query
      InspectionModuleNotFound moduleName ->
        encodeListLen 2 <> encodeString "ModuleNotFound" <> text moduleName
      InspectionRejected diagnostic ->
        encodeListLen 2 <> encodeString "Rejected" <> text diagnostic
      InspectionBrowse moduleName expanded entries ->
        encodeListLen 4
          <> encodeString "Browse"
          <> text moduleName
          <> encodeBool expanded
          <> encodeEntries entries
      InspectionTypeMatches query matches ->
        encodeListLen 3
          <> encodeString "TypeMatches"
          <> text query
          <> encodeTypeMatches matches
      InspectionStructuredInfo details ->
        encodeListLen 2 <> encodeString "StructuredInfoOk" <> encodeIdentifierInfo details
      InspectionStructuredType details ->
        encodeListLen 2 <> encodeString "StructuredTypeOk" <> encodeTypeInfo details
      InspectionStructuredError failure ->
        encodeListLen 2 <> encodeString "StructuredError" <> encodeStructuredError failure
    encodeEntries entries =
      encodeListLen (fromIntegral (length entries)) <> foldMap encodeEntry entries
    encodeEntry entry =
      encodeListLen 5
        <> text (infoName entry)
        <> maybe encodeNull text (infoModule entry)
        <> text (infoKind entry)
        <> text (infoDisplay entry)
        <> encodeAvailability (infoAvailability entry)
    encodeTypeMatches matches =
      encodeListLen (fromIntegral (length matches)) <> foldMap encodeTypeMatch matches
    encodeTypeMatch match =
      encodeListLen 5
        <> text (typeMatchName match)
        <> maybe encodeNull text (typeMatchModule match)
        <> text (typeMatchSignature match)
        <> encodeString (case typeMatchQuality match of
          TypeMatchExact -> "Exact"
          TypeMatchUsable -> "Usable")
        <> encodeAvailability (typeMatchAvailability match)
    encodeAvailability = encodeString . T.pack . show
    text = encodeString . T.pack

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
    StructuredConstructorName -> "Constructor")
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
