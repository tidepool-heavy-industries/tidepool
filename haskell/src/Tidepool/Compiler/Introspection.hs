module Tidepool.Compiler.Introspection
  ( InspectionResult(..)
  , InfoEntry(..)
  , IdentifierNamespace(..)
  , IdentifierRef(..)
  , ScopeProvenance(..)
  , TypeExpression(..)
  , TypeInfo(..)
  , FieldInfo(..)
  , ConstructorInfo(..)
  , ClassMethodInfo(..)
  , DeclarationInfo(..)
  , IdentifierInfo(..)
  , StructuredQueryError(..)
  , runInspection
  , encodeInspectionResults
  ) where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Control.Monad (foldM, forM)
import Control.Monad.IO.Class (liftIO)
import qualified Data.ByteString as BS
import Data.List (nub, nubBy, sortOn)
import qualified Data.Map.Strict as Map
import Data.Maybe (catMaybes, isJust)
import qualified Data.Text as T
import Data.Word (Word64)
import GHC
import GHC.Core.Class (classTyVars)
import GHC.Core.ConLike (ConLike(..))
import GHC.Core.DataCon
  ( dataConDisplayType, dataConFieldType, dataConOrigArgTys )
import GHC.Core.Multiplicity (scaledThing)
import GHC.Core.TyCon (isAlgTyCon)
import GHC.Iface.Type (ShowForAllFlag(..), ShowHowMuch(..), ShowSub(..))
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Types.FieldLabel (flLabel, flSelector)
import GHC.Types.Name
  ( isDataConName, isTyConName, isVarName, nameModule_maybe, nameOccName )
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Name.Reader (GlobalRdrEnv, globalRdrEnvElts, greName)
import GHC.Types.TyThing (tyThingParent_maybe)
import GHC.Types.TyThing.Ppr (pprTyThing, pprTyThingInContext)
import GHC.Types.Var (varName)
import GHC.Utils.Outputable (Outputable, defaultSDocContext, ppr, renderWithContext)
import Tidepool.ExtractRequest
  ( InspectionProvenance(..), InspectionRequest(..), StructuredInspection(..)
  , StructuredNameNamespace(..), StructuredNameScope(..)
  )
import Tidepool.ExtractUtil (getLibdir)

data InfoEntry = InfoEntry
  { infoName :: String
  , infoModule :: Maybe String
  , infoKind :: String
  , infoDisplay :: String
  }
  deriving (Eq, Show)

data IdentifierNamespace
  = ValueIdentifier
  | TypeIdentifier
  | ConstructorIdentifier
  | FieldIdentifier
  deriving (Eq, Show)

data IdentifierRef = IdentifierRef
  { identifierModule :: String
  , identifierName :: String
  , identifierNamespace :: IdentifierNamespace
  }
  deriving (Eq, Show)

data ScopeProvenance = ScopeProvenance
  { provenanceScope :: StructuredNameScope
  , provenanceGeneration :: Word64
  , provenanceFingerprint :: String
  }
  deriving (Eq, Show)

data TypeExpression = TypeExpression
  { typeCanonical :: String
  , typeVariables :: [String]
  , typeConstraints :: [String]
  }
  deriving (Eq, Show)

data TypeInfo = TypeInfo
  { typeIdentifier :: IdentifierRef
  , typeExpression :: TypeExpression
  , typeProvenance :: ScopeProvenance
  }
  deriving (Eq, Show)

data FieldInfo = FieldInfo
  { fieldName :: String
  , fieldType :: TypeExpression
  }
  deriving (Eq, Show)

data ConstructorInfo = ConstructorInfo
  { constructorRef :: IdentifierRef
  , constructorType :: TypeExpression
  , constructorArguments :: [TypeExpression]
  , recordFields :: [FieldInfo]
  }
  deriving (Eq, Show)

data ClassMethodInfo = ClassMethodInfo
  { classMethodRef :: IdentifierRef
  , classMethodType :: TypeExpression
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
  { inspectedIdentifier :: IdentifierRef
  , identifierDeclaration :: DeclarationInfo
  , identifierParent :: Maybe IdentifierRef
  , identifierProvenance :: ScopeProvenance
  }
  deriving (Eq, Show)

data StructuredQueryError
  = StructuredUnknown StructuredInspection
  | StructuredAmbiguous StructuredInspection [IdentifierRef]
  | StructuredUnknownModule String
  | StructuredUnsupported String
  deriving (Eq, Show)

data InspectionResult
  = InspectionType String String
  | InspectionInfo String [InfoEntry]
  | InspectionAmbiguous String [InfoEntry]
  | InspectionNotFound String
  | InspectionModuleNotFound String
  | InspectionRejected String
  | InspectionBrowse String Bool [InfoEntry]
  | InspectionStructuredInfo IdentifierInfo
  | InspectionStructuredType TypeInfo
  | InspectionStructuredError StructuredQueryError
  deriving (Eq, Show)

runInspection
  :: HscEnv
  -> GlobalRdrEnv
  -> Map.Map String String
  -> [InspectionRequest]
  -> IO [InspectionResult]
runInspection hscEnv rdrEnv capturedTypes requests = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    setSession hscEnv
    snd <$> foldM inspect (0 :: Int, []) requests
  where
    inspect (typeIndex, results) request = case request of
      InspectTypeOf expression ->
        let binder = "__tidepool_inspect_" ++ show typeIndex
        in case Map.lookup binder capturedTypes of
          Just display -> pure (typeIndex + 1, results ++ [InspectionType expression display])
          Nothing -> liftIO (ioError (userError ("inspection module did not expose " ++ binder)))
      InspectNameInfo query -> do
        result <- inspectName rdrEnv query
        pure (typeIndex, results ++ [result])
      InspectModule moduleName expanded -> do
        result <- inspectModule moduleName expanded
        pure (typeIndex, results ++ [result])
      InspectStructuredInfoOf query -> do
        result <- inspectStructured rdrEnv StructuredInfo query
        pure (typeIndex, results ++ [result])
      InspectStructuredTypeOf query -> do
        result <- inspectStructured rdrEnv StructuredType query
        pure (typeIndex, results ++ [result])

data StructuredMode = StructuredInfo | StructuredType

inspectStructured
  :: GhcMonad m
  => GlobalRdrEnv
  -> StructuredMode
  -> StructuredInspection
  -> m InspectionResult
inspectStructured rdrEnv mode query = do
  candidates <- namesInScope rdrEnv query
  case candidates of
    Left missingModule ->
      pure (InspectionStructuredError (StructuredUnknownModule missingModule))
    Right (names, visibleNames) -> do
      things <- fmap catMaybes $ forM names $ \name -> do
        found <- lookupName name
        pure ((name,) <$> found)
      case things of
        [] -> pure (InspectionStructuredError (StructuredUnknown query))
        [resolved] -> pure (inspectResolved mode query visibleNames resolved)
        many -> pure (InspectionStructuredError
          (StructuredAmbiguous query (map (uncurry identifierRef) many)))

namesInScope
  :: GhcMonad m
  => GlobalRdrEnv
  -> StructuredInspection
  -> m (Either String ([Name], [Name]))
namesInScope rdrEnv query = case structuredScope query of
  StructuredCurrentScope -> pure (Right (filter matches currentNames, currentNames))
  StructuredPublicModule requested -> handleSourceError
    (\_ -> pure (Left requested)) $ do
      mdl <- findModule (mkModuleName requested) Nothing
      resolvedInfo <- getModuleInfo mdl
      let visible = nub (maybe [] modInfoExports resolvedInfo)
      pure (Right (filter matches visible, visible))
  where
    currentNames = nub (map greName (globalRdrEnvElts rdrEnv))
    matches name = matchesQuery (structuredName query) name
      && namespaceMatches (structuredNamespace query) name

namespaceMatches :: StructuredNameNamespace -> Name -> Bool
namespaceMatches namespace name = case namespace of
  StructuredAnyName -> True
  StructuredValueName -> isVarName name
  StructuredTypeName -> isTyConName name
  StructuredConstructorName -> isDataConName name

inspectResolved
  :: StructuredMode
  -> StructuredInspection
  -> [Name]
  -> (Name, TyThing)
  -> InspectionResult
inspectResolved mode query visibleNames (name, thing) =
  case mode of
    StructuredInfo -> case identifierInfo query visibleNames name thing of
      Left detail -> InspectionStructuredError (StructuredUnsupported detail)
      Right details -> InspectionStructuredInfo details
    StructuredType -> case typeForThing thing of
      Nothing -> InspectionStructuredError (StructuredUnsupported (unsupportedThing thing))
      Just ty -> InspectionStructuredType TypeInfo
        { typeIdentifier = identifierRef name thing
        , typeExpression = describeType ty
        , typeProvenance = queryProvenance query
        }

identifierInfo
  :: StructuredInspection
  -> [Name]
  -> Name
  -> TyThing
  -> Either String IdentifierInfo
identifierInfo query visible name thing = do
  declaration <- declarationInfo visible thing
  let parent = tyThingParent_maybe thing
  pure IdentifierInfo
    { inspectedIdentifier = identifierRef name thing
    , identifierDeclaration = declaration
    , identifierParent = identifierRefForThing <$> parent
    , identifierProvenance = queryProvenance query
    }

declarationInfo :: [Name] -> TyThing -> Either String DeclarationInfo
declarationInfo visible thing = case thing of
  AnId identifier
    | isRecordSelector identifier -> case tyThingParent_maybe thing of
        Just parent -> Right (RecordSelectorDeclaration
          (identifierRefForThing parent) (describeType (idType identifier)))
        Nothing -> Left "record selector has no parent declaration"
    | otherwise -> Right (ValueDeclaration (describeType (idType identifier)))
  AConLike (RealDataCon constructor) -> case tyThingParent_maybe thing of
    Just parent -> Right (ConstructorDeclaration
      (identifierRefForThing parent) (describeConstructor visible constructor))
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
        (map (render . varName) (tyConTyVars tyCon))
        (describeType rhs))
    | isAlgTyCon tyCon ->
        let constructors = map (describeConstructor visible)
              (filter ((`elem` visible) . getName) (tyConDataCons tyCon))
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
  { constructorRef = identifierRef
      (getName constructor) (AConLike (RealDataCon constructor))
  , constructorType = describeType (dataConDisplayType False constructor)
  , constructorArguments =
      map (describeType . scaledThing) (dataConOrigArgTys constructor)
  , recordFields = map (\field -> FieldInfo
      { fieldName = render (flLabel field)
      , fieldType = describeType (dataConFieldType constructor (flLabel field))
      }) (filter fieldVisible (dataConFieldLabels constructor))
  }
  where
    fieldVisible field = flSelector field `elem` visible

describeMethod :: Id -> ClassMethodInfo
describeMethod method = ClassMethodInfo
  { classMethodRef = identifierRef (getName method) (AnId method)
  , classMethodType = describeType (idType method)
  }

typeForThing :: TyThing -> Maybe Type
typeForThing thing = case thing of
  AnId identifier -> Just (idType identifier)
  AConLike (RealDataCon constructor) -> Just (dataConDisplayType False constructor)
  AConLike (PatSynCon _) -> Nothing
  ATyCon tyCon -> Just (tyConKind tyCon)
  ACoAxiom _ -> Nothing

describeType :: Type -> TypeExpression
describeType ty = TypeExpression
  { typeCanonical = render ty
  , typeVariables = map (render . varName) variables
  , typeConstraints = map render constraints
  }
  where
    (variables, constraints, _) = tcSplitSigmaTy ty

identifierRefForThing :: TyThing -> IdentifierRef
identifierRefForThing thing = identifierRef (getName thing) thing

identifierRef :: Name -> TyThing -> IdentifierRef
identifierRef name thing = IdentifierRef
  { identifierModule = maybe "" (moduleNameString . moduleName) (nameModule_maybe name)
  , identifierName = occNameString (nameOccName name)
  , identifierNamespace = case thing of
      AnId identifier | isRecordSelector identifier -> FieldIdentifier
      AnId _ -> ValueIdentifier
      AConLike _ -> ConstructorIdentifier
      ATyCon _ -> TypeIdentifier
      ACoAxiom _ -> TypeIdentifier
  }

queryProvenance :: StructuredInspection -> ScopeProvenance
queryProvenance query = ScopeProvenance
  { provenanceScope = structuredScope query
  , provenanceGeneration = inspectionGeneration (structuredProvenance query)
  , provenanceFingerprint = inspectionFingerprint (structuredProvenance query)
  }

unsupportedThing :: TyThing -> String
unsupportedThing thing = "unsupported declaration: " ++ thingKind thing

inspectName :: GhcMonad m => GlobalRdrEnv -> String -> m InspectionResult
inspectName rdrEnv query = do
  let names = [ greName gre
              | gre <- globalRdrEnvElts rdrEnv
              , matchesQuery query (greName gre) ]
  entries <- entriesFor names
  let preferred = if any ((== "type") . infoKind) entries
        then filter ((/= "constructor") . infoKind) entries
        else entries
  pure $ case preferred of
    [] -> InspectionNotFound query
    [_] -> InspectionInfo query preferred
    _ -> InspectionAmbiguous query preferred

inspectModule :: GhcMonad m => String -> Bool -> m InspectionResult
inspectModule requested expanded = handleSourceError
  (\_ -> pure (InspectionModuleNotFound requested)) $ do
  mdl <- findModule (mkModuleName requested) Nothing
  resolvedInfo <- getModuleInfo mdl
  let names = maybe [] modInfoExports resolvedInfo
  rawEntries <- browseEntries expanded names
  let entries = nubBy sameDisplay (sortOn entryKey rawEntries)
  pure (InspectionBrowse (moduleNameString (moduleName mdl)) expanded entries)
  where
    sameDisplay left right = infoDisplay left == infoDisplay right
    entryKey entry = (infoName entry, infoKind entry, infoDisplay entry)

entriesFor :: GhcMonad m => [Name] -> m [InfoEntry]
entriesFor names = fmap concat $ forM names $ \name -> do
  found <- getInfo False name
  pure $ case found of
    Nothing -> []
    Just (thing, _fixity, _instances, _families, _extra) ->
      let display = renderWithContext defaultSDocContext
            (pprTyThingInContext showEverything thing)
          definingModule = moduleNameString . moduleName <$> nameModule_maybe name
      in [InfoEntry
            { infoName = occNameString (nameOccName name)
            , infoModule = definingModule
            , infoKind = thingKind thing
            , infoDisplay = display
            }]

browseEntries :: GhcMonad m => Bool -> [Name] -> m [InfoEntry]
browseEntries expanded names = do
  found <- fmap catMaybes $ forM names $ \name -> do
    thing <- lookupName name
    pure (fmap (\value -> (name, value)) thing)
  let exportedNames = map fst found
      visible = if expanded
        then found
        else filter (not . hasExportedParent exportedNames . snd) found
  pure $ flip map visible $ \(name, thing) ->
      let document = if expanded
            then pprTyThing showEverything thing
            else pprTyThingInContext showEverything thing
          display = renderWithContext defaultSDocContext document
          definingModule = moduleNameString . moduleName <$> nameModule_maybe name
      in InfoEntry
            { infoName = occNameString (nameOccName name)
            , infoModule = definingModule
            , infoKind = thingKind thing
            , infoDisplay = display
            }
  where
    hasExportedParent exported thing = case tyThingParent_maybe thing of
      Just parent -> getName parent `elem` exported
      Nothing -> False

matchesQuery :: String -> Name -> Bool
matchesQuery query name =
  occNameString (nameOccName name) == occurrence
    && maybe True (\wanted -> definingModule == Just wanted) qualifier
  where
    (qualifier, occurrence) = case break (== '.') (reverse query) of
      (reversedOccurrence, []) -> (Nothing, reverse reversedOccurrence)
      (reversedOccurrence, _ : reversedQualifier) ->
        (Just (reverse reversedQualifier), reverse reversedOccurrence)
    definingModule = moduleNameString . moduleName <$> nameModule_maybe name

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

-- | Private V3 batch receipt. Legacy result bodies retain their V2 shape.
encodeInspectionResults :: [InspectionResult] -> BS.ByteString
encodeInspectionResults results = toStrictByteString $
  encodeListLen 2 <> encodeString "TPINSP003"
    <> encodeListLen (fromIntegral (length results)) <> foldMap encodeResult results
  where
    encodeResult inspection = case inspection of
      InspectionType expression display ->
        encodeListLen 3 <> encodeString "Type" <> text expression <> text display
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
        encodeListLen 4 <> encodeString "Browse" <> text moduleName
          <> encodeBool expanded <> encodeEntries entries
      InspectionStructuredInfo details ->
        encodeListLen 2 <> encodeString "StructuredInfoOk"
          <> encodeIdentifierInfo details
      InspectionStructuredType details ->
        encodeListLen 2 <> encodeString "StructuredTypeOk"
          <> encodeTypeInfo details
      InspectionStructuredError failure ->
        encodeListLen 2 <> encodeString "StructuredError"
          <> encodeStructuredError failure
    encodeEntries entries =
      encodeListLen (fromIntegral (length entries)) <> foldMap encodeEntry entries
    encodeEntry entry =
      encodeListLen 4 <> text (infoName entry) <> maybe encodeNull text (infoModule entry)
        <> text (infoKind entry) <> text (infoDisplay entry)
    text = encodeString . T.pack

encodeStructuredError :: StructuredQueryError -> Encoding
encodeStructuredError failure = case failure of
  StructuredUnknown query ->
    encodeListLen 2 <> encodeString "Unknown" <> encodeStructuredQuery query
  StructuredAmbiguous query candidates ->
    encodeListLen 3 <> encodeString "Ambiguous" <> encodeStructuredQuery query
      <> encodeList encodeIdentifierRef candidates
  StructuredUnknownModule moduleName ->
    encodeListLen 2 <> encodeString "UnknownModule" <> encodeText moduleName
  StructuredUnsupported detail ->
    encodeListLen 2 <> encodeString "Unsupported" <> encodeText detail

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
  ValueDeclaration ty ->
    encodeListLen 2 <> encodeString "Value" <> encodeTypeExpression ty
  DataDeclaration parameters constructors ->
    encodeListLen 3 <> encodeString "Data" <> encodeTexts parameters
      <> encodeList encodeConstructor constructors
  NewtypeDeclaration parameters constructor ->
    encodeListLen 3 <> encodeString "Newtype" <> encodeTexts parameters
      <> encodeConstructor constructor
  TypeSynonymDeclaration parameters rhs ->
    encodeListLen 3 <> encodeString "TypeSynonym" <> encodeTexts parameters
      <> encodeTypeExpression rhs
  ClassDeclaration parameters supers methods ->
    encodeListLen 4 <> encodeString "Class" <> encodeTexts parameters
      <> encodeList encodeTypeExpression supers <> encodeList encodeClassMethod methods
  ConstructorDeclaration parent constructor ->
    encodeListLen 3 <> encodeString "Constructor" <> encodeIdentifierRef parent
      <> encodeConstructor constructor
  RecordSelectorDeclaration parent ty ->
    encodeListLen 3 <> encodeString "RecordSelector" <> encodeIdentifierRef parent
      <> encodeTypeExpression ty

encodeConstructor :: ConstructorInfo -> Encoding
encodeConstructor constructor = encodeListLen 4
  <> encodeIdentifierRef (constructorRef constructor)
  <> encodeTypeExpression (constructorType constructor)
  <> encodeList encodeTypeExpression (constructorArguments constructor)
  <> encodeList encodeField (recordFields constructor)

encodeField :: FieldInfo -> Encoding
encodeField field = encodeListLen 2 <> encodeText (fieldName field)
  <> encodeTypeExpression (fieldType field)

encodeClassMethod :: ClassMethodInfo -> Encoding
encodeClassMethod method = encodeListLen 2
  <> encodeIdentifierRef (classMethodRef method)
  <> encodeTypeExpression (classMethodType method)

encodeTypeExpression :: TypeExpression -> Encoding
encodeTypeExpression ty = encodeListLen 3 <> encodeText (typeCanonical ty)
  <> encodeTexts (typeVariables ty) <> encodeTexts (typeConstraints ty)

encodeProvenance :: ScopeProvenance -> Encoding
encodeProvenance provenance = encodeListLen 3
  <> encodeScope (provenanceScope provenance)
  <> encodeWord64 (provenanceGeneration provenance)
  <> encodeText (provenanceFingerprint provenance)

encodeStructuredQuery :: StructuredInspection -> Encoding
encodeStructuredQuery query = encodeListLen 3
  <> encodeScope (structuredScope query)
  <> encodeString (case structuredNamespace query of
      StructuredAnyName -> "Any"
      StructuredValueName -> "Value"
      StructuredTypeName -> "Type"
      StructuredConstructorName -> "Constructor")
  <> encodeText (structuredName query)

encodeScope :: StructuredNameScope -> Encoding
encodeScope scope = case scope of
  StructuredCurrentScope -> encodeListLen 1 <> encodeString "Current"
  StructuredPublicModule moduleName ->
    encodeListLen 2 <> encodeString "PublicModule" <> encodeText moduleName

encodeIdentifierRef :: IdentifierRef -> Encoding
encodeIdentifierRef identifier = encodeListLen 3
  <> encodeText (identifierModule identifier)
  <> encodeText (identifierName identifier)
  <> encodeString (case identifierNamespace identifier of
      ValueIdentifier -> "Value"
      TypeIdentifier -> "Type"
      ConstructorIdentifier -> "Constructor"
      FieldIdentifier -> "Field")

encodeTexts :: [String] -> Encoding
encodeTexts = encodeList encodeText

encodeList :: (value -> Encoding) -> [value] -> Encoding
encodeList encode values =
  encodeListLen (fromIntegral (length values)) <> foldMap encode values

encodeText :: String -> Encoding
encodeText = encodeString . T.pack
