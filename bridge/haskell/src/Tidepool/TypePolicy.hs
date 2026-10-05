module Tidepool.TypePolicy
  ( isGhcCompilerName
  , isGhcCompilerTyCon
  , modulesOfType
  , nominalHeadsOfType
  , rootNominalHeadOfType
  , NominalHead(..)
  , TypeNodeId(..)
  , TypeNodeG
  , TypeGraph, tgNodes, tgEdges, emptyTypeGraph
  , TypeGraphError(..)
  , TypeGraphBuilder
  , emptyTypeGraphBuilder
  , internType, internConstructorType
  , finishTypeGraph
  , stabilizeEffectRows
  ) where

import Control.Monad.State.Strict
import Control.Exception (Exception)
import qualified Data.ByteString as BS
import Data.Char (isDigit)
import Data.List (elemIndex)
import qualified Data.Map.Strict as Map
import qualified Data.Text.Encoding as TextEncoding
import Tidepool.ExecutionSchema (TypeNodeId(..))
import qualified Tidepool.ExecutionSchema as W
import Data.IntMap.Strict (IntMap)
import qualified Data.IntMap.Strict as IntMap
import Data.Text (Text)
import qualified Data.List as List
import Data.Maybe (mapMaybe, maybeToList)
import qualified Data.Text as T
import Data.Word (Word32)
import GHC.Builtin.Names (integerTyConKey, naturalTyConKey)
import GHC.Core.DataCon
  ( DataCon, dataConOrigArgTys, dataConUnivTyVars, dataConUserTyVarBinders
  , dataConTag, dataConName, dataConTyCon, dataConRepArgTys, dataConOrigResTy
  , dataConRepStrictness, dataConImplBangs, HsImplBang(..), isMarkedStrict, isVanillaDataCon )
import GHC.Core.TyCo.FVs (tyConsOfType)
import GHC.Core.TyCo.Rep (Scaled(..), Type(..), TyLit(..))
import GHC.Core.TyCon
  ( TyCon, isAlgTyCon, isFamilyTyCon, isNewTyCon, isPrimTyCon
  , isTypeSynonymTyCon, newTyConEtadRhs, tyConDataCons, tyConName, tyConUnique, tyConFamilySize
  , tyConBinders, TyConBndrVis(..) )
import GHC.Core.Type (coreView, expandTypeSynonyms, mkTyConApp, mkTyVarTy
  , splitTyConApp_maybe, typeKind, sORTKind_maybe)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Types.RepType (PrimRep(..), typePrimRep_maybe)
import GHC.Types.Name (Name, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (isDataOcc, occNameString)
import GHC.Types.Var (TyVar, VarBndr(..), ForAllTyFlag(..), Specificity(..)
  , FunTyFlag(..), isTyVar, varType)
import GHC.Data.FastString (unpackFS)
import qualified GHC.Types.Unique.Set as USet
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (moduleUnitId, unitIdString, unitString)
import GHC.Utils.Outputable (SDocContext(sdocSuppressUniques), defaultSDocContext, ppr, renderWithContext)

data NominalHead = NominalHead
  { nhUnit :: Text
  , nhModule :: Text
  , nhName :: Text
  }
  deriving (Eq, Ord, Show)

-- Compiler payloads share the finite wire graph grammar. No field is
-- instantiated here: declaration templates retain original GHC parameters.
type TypeNodeG = W.TypeNodeF DataCon TyCon PrimRep
type TypeGraph = W.TypeGraphF DataCon TyCon PrimRep

tgNodes :: TypeGraph -> IntMap TypeNodeG
tgNodes = W.typeGraphNodes

tgEdges :: TypeGraph -> IntMap [(W.TypeEdgeRoleF PrimRep, TypeNodeId)]
tgEdges = W.typeGraphEdges

emptyTypeGraph :: TypeGraph
emptyTypeGraph = W.TypeGraph IntMap.empty IntMap.empty

data TypeGraphError
  = TypeGraphNodeLimit | TypeGraphEdgeLimit | TypeGraphByteLimit | TypeGraphWorkLimit
  | TypeGraphFreeVariable | TypeGraphLocalName | TypeGraphCoercionBinder
  | TypeGraphCast | TypeGraphCoercion | TypeGraphIncompleteReservation
  | TypeGraphOriginalDeclarationMismatch
  deriving (Eq, Ord, Show)

instance Exception TypeGraphError

-- Indexes belong to this one builder. Expression keys are context-free
-- de-Bruijn syntax; root/declaration telescopes give their scoped meaning.
data ExpressionKey
  = BoundKey Word32 | NominalKey TypeNodeId [TypeNodeId]
  | ApplicationKey TypeNodeId TypeNodeId
  | FunctionKey W.FunctionFlag TypeNodeId TypeNodeId TypeNodeId
  | ForAllKey W.ForAllFlag TypeNodeId TypeNodeId | LiteralKey W.TypeLiteral
  | RootKey W.RootDomain [W.SourceBinderFlag] [TypeNodeId] TypeNodeId
  | TemplateKey (NominalHead, Bool) (NominalHead, Bool) Int Int
      [Maybe [PrimRep]] [Bool] (Maybe [PrimRep]) [Bool] [(TypeNodeId, PrimRep)]
  deriving (Eq, Ord)

data TypeGraphBuilder = TypeGraphBuilder
  { tgbExpressions :: !(Map.Map ExpressionKey TypeNodeId)
  , tgbDeclarations :: !(Map.Map (NominalHead, Bool) TypeNodeId)
  , tgbNodes :: !(IntMap (Maybe TypeNodeG))
  , tgbEdges :: !(IntMap [(W.TypeEdgeRoleF PrimRep, TypeNodeId)])
  , tgbActiveDeclarations :: !(Map.Map (NominalHead, Bool) TypeNodeId)
  , tgbNext :: !Word32
  , tgbEdgeCount :: !Int
  , tgbBytes :: !Int
  , tgbWork :: !Int
  }

type GraphBuild = StateT TypeGraphBuilder (Either TypeGraphError)

emptyTypeGraphBuilder :: TypeGraphBuilder
emptyTypeGraphBuilder = TypeGraphBuilder Map.empty Map.empty IntMap.empty IntMap.empty Map.empty 0 0 0 0

finishTypeGraph :: TypeGraphBuilder -> Either TypeGraphError TypeGraph
finishTypeGraph builder
  | not (Map.null (tgbActiveDeclarations builder)) = Left TypeGraphIncompleteReservation
  | IntMap.size (tgbNodes builder) /= fromIntegral (tgbNext builder) = Left TypeGraphIncompleteReservation
  | otherwise = do
      nodes <- traverse completed [0 .. fromIntegral (tgbNext builder) - 1]
      pure (W.TypeGraph (IntMap.fromAscList (zip [0 :: Int ..] nodes))
        (IntMap.map (List.sortOn fst) (tgbEdges builder)))
 where
  completed index = case IntMap.lookup index (tgbNodes builder) of
    Just (Just node) -> Right node
    _ -> Left TypeGraphIncompleteReservation

internType :: Type -> GraphBuild TypeNodeId
internType = internRoot W.ClosedRoot []

internConstructorType :: DataCon -> Type -> GraphBuild TypeNodeId
internConstructorType constructor = internRoot W.ConstructorSchemeRoot
  [(variable, sourceFlag specificity)
    | Bndr variable specificity <- dataConUserTyVarBinders constructor]
 where
  sourceFlag SpecifiedSpec = W.SourceSpecified
  sourceFlag InferredSpec = W.SourceInferred

internRoot :: W.RootDomain -> [(TyVar, W.SourceBinderFlag)] -> Type -> GraphBuild TypeNodeId
internRoot domain binders original = do
  (bound, kinds) <- binderKinds (map fst binders)
  body <- internExpression bound original
  let flags = map snd binders
      key = RootKey domain flags kinds body
  internExpressionNode key (W.TypeRoot domain flags (renderType original))
    ([(W.TypeBinderKind (fromIntegral index), kind) | (index, kind) <- zip [0 :: Int ..] kinds]
      ++ [(W.TypeBody, body)])

binderKinds :: [TyVar] -> GraphBuild ([TyVar], [TypeNodeId])
binderKinds = go [] []
 where
  go bound kinds [] = pure (bound, reverse kinds)
  go bound kinds (variable : rest)
    | not (isTyVar variable) = lift (Left TypeGraphCoercionBinder)
    | otherwise = do
        kind <- internExpression bound (varType variable)
        go (variable : bound) (kind : kinds) rest

-- Synonyms retain the existing transparent identity policy. Declaration links
-- reserve before following templates, so both regular and nonregular recursion
-- close without substituting fields or imposing an expansion depth.
internExpression :: [TyVar] -> Type -> GraphBuild TypeNodeId
internExpression bound original = do
  chargeWork 1
  case coreView original of
    Just expanded -> internExpression bound expanded
    Nothing -> case original of
      TyVarTy variable -> case elemIndex variable bound of
        Just index | isTyVar variable -> internExpressionNode (BoundKey (fromIntegral index))
          (W.TypeBound (fromIntegral index)) []
        _ -> lift (Left TypeGraphFreeVariable)
      TyConApp constructor arguments -> do
        declaration <- internDeclaration constructor
        children <- traverse (internExpression bound) arguments
        internExpressionNode (NominalKey declaration children) W.TypeNominalApplication
          ((W.TypeHead, declaration) :
            [(W.TypeArgument (fromIntegral index), child) | (index, child) <- zip [0 :: Int ..] children])
      AppTy function argument -> do
        functionNode <- internExpression bound function
        argumentNode <- internExpression bound argument
        internExpressionNode (ApplicationKey functionNode argumentNode) W.TypeApplication
          [(W.TypeFunctionEdge, functionNode), (W.TypeApplyArgument, argumentNode)]
      FunTy flag multiplicity argument result -> do
        mult <- internExpression bound multiplicity
        domain <- internExpression bound argument
        codomain <- internExpression bound result
        let projected = case flag of
              FTF_T_T -> W.TypeToType; FTF_T_C -> W.TypeToConstraint
              FTF_C_T -> W.ConstraintToType; FTF_C_C -> W.ConstraintToConstraint
        internExpressionNode (FunctionKey projected mult domain codomain) (W.TypeFunction projected)
          [(W.TypeMultiplicity, mult), (W.TypeDomain, domain), (W.TypeCodomain, codomain)]
      ForAllTy (Bndr variable flag) body
        | isTyVar variable -> do
            kind <- internExpression bound (varType variable)
            bodyNode <- internExpression (variable : bound) body
            let projected = case flag of
                  Required -> W.ForAllRequired
                  Invisible SpecifiedSpec -> W.ForAllSpecified
                  Invisible InferredSpec -> W.ForAllInferred
            internExpressionNode (ForAllKey projected kind bodyNode) (W.TypeForAll projected)
              [(W.TypeKind, kind), (W.TypeBody, bodyNode)]
        | otherwise -> lift (Left TypeGraphCoercionBinder)
      LitTy literal -> do
        let projected = case literal of
              NumTyLit value -> W.NaturalTypeLiteral (T.pack (show value))
              StrTyLit value -> W.SymbolTypeLiteral (T.pack (unpackFS value))
              CharTyLit value -> W.CharacterTypeLiteral value
        internExpressionNode (LiteralKey projected) (W.TypeLiteral projected) []
      CastTy{} -> lift (Left TypeGraphCast)
      CoercionTy{} -> lift (Left TypeGraphCoercion)

internExpressionNode :: ExpressionKey -> TypeNodeG
  -> [(W.TypeEdgeRoleF PrimRep, TypeNodeId)] -> GraphBuild TypeNodeId
internExpressionNode key node children = do
  known <- gets (Map.lookup key . tgbExpressions)
  case known of
    Just identity -> pure identity
    Nothing -> do
      identity <- reserveNode
      modify' (\builder -> builder
        { tgbExpressions = Map.insert key identity (tgbExpressions builder) })
      completeNode identity node
      mapM_ (uncurry (addEdge identity)) children
      pure identity

internDeclaration :: TyCon -> GraphBuild TypeNodeId
internDeclaration constructor = do
  key <- lift (declarationKey constructor)
  active <- gets (Map.lookup key . tgbActiveDeclarations)
  case active of
    Just identity -> pure identity
    Nothing -> do
      known <- gets (Map.lookup key . tgbDeclarations)
      identity <- case known of
        Just existing -> pure existing
        Nothing -> do
          reserved <- reserveNode
          modify' (\builder -> builder
            { tgbDeclarations = Map.insert key reserved (tgbDeclarations builder) })
          pure reserved
      previous <- gets (IntMap.lookup (nodeIndex identity) . tgbNodes)
      outgoing <- gets (IntMap.findWithDefault [] (nodeIndex identity) . tgbEdges)
      modify' (\builder -> builder
        { tgbActiveDeclarations = Map.insert key identity (tgbActiveDeclarations builder)
        , tgbEdges = IntMap.delete (nodeIndex identity) (tgbEdges builder)
        , tgbEdgeCount = tgbEdgeCount builder - length outgoing
        , tgbBytes = tgbBytes builder - 32 * length outgoing })
      let binders = tyConBinders constructor
          variables = [variable | Bndr variable _ <- binders]
          flags = [parameterFlag flag | Bndr _ flag <- binders]
      (_, kinds) <- binderKinds variables
      mapM_ (\(index, kind) -> addEdge identity (W.TypeBinderKind (fromIntegral index)) kind)
        (zip [0 :: Int ..] kinds)
      form <- declarationForm identity constructor variables
      let restriction = if isSpecial "Control.Monad.Freer.Internal" "Eff" constructor
            then W.EffectHead else W.UnrestrictedSyntax
          node = W.TypeDeclaration constructor flags form restriction
      case previous of
        Just (Just (W.TypeDeclaration _ oldFlags oldForm oldRestriction)) -> do
          currentOutgoing <- gets (IntMap.findWithDefault [] (nodeIndex identity) . tgbEdges)
          if oldFlags == flags && oldForm == form && oldRestriction == restriction
              && outgoing == currentOutgoing
            then modify' (\builder -> builder
              { tgbEdges = IntMap.insert (nodeIndex identity) outgoing (tgbEdges builder) })
            else lift (Left TypeGraphOriginalDeclarationMismatch)
        Just Nothing -> completeNode identity node
        _ -> lift (Left TypeGraphIncompleteReservation)
      modify' (\builder -> builder
        { tgbActiveDeclarations = Map.delete key (tgbActiveDeclarations builder) })
      pure identity
 where
  parameterFlag (NamedTCB Required) = W.NamedRequired
  parameterFlag (NamedTCB (Invisible SpecifiedSpec)) = W.NamedSpecified
  parameterFlag (NamedTCB (Invisible InferredSpec)) = W.NamedInferred
  parameterFlag AnonTCB = W.AnonymousVisible

declarationKey :: TyCon -> Either TypeGraphError (NominalHead, Bool)
declarationKey = nameKey . tyConName

nameKey :: Name -> Either TypeGraphError (NominalHead, Bool)
nameKey name = case nameModule_maybe name of
  Nothing -> Left TypeGraphLocalName
  Just owner -> Right (NominalHead
    (T.pack (unitString (moduleUnit owner)))
    (T.pack (moduleNameString (moduleName owner)))
    (T.pack (occNameString (nameOccName name))),
    isDataOcc (nameOccName name))

declarationForm :: TypeNodeId -> TyCon -> [TyVar] -> GraphBuild (W.DeclarationFormF PrimRep)
declarationForm identity constructor variables
  | isFamilyTyCon constructor = pure (opaque W.NominalFamily "type family")
  | isNewTyCon constructor = do
      let (etaVariables, rhs) = newTyConEtadRhs constructor
      if etaVariables /= take (length etaVariables) variables
        then lift (Left TypeGraphOriginalDeclarationMismatch)
        else do
          -- RHS uses only the eta-prefix; trailing arguments are reapplied by
          -- the shared runtime weak-head owner, never by graph expansion.
          body <- internExpression (reverse etaVariables) rhs
          addEdge identity W.TypeAliasRhs body
          pure (W.NewtypeDeclaration (fromIntegral (length etaVariables)))
  | isSpecial "Data.Text.Internal" "Text" constructor = pure W.TextDeclaration
  | tyConUnique constructor == integerTyConKey = pure W.IntegerDeclaration
  | tyConUnique constructor == naturalTyConKey = pure W.NaturalDeclaration
  | isForbidden constructor = pure (opaque W.NominalConstructor "unsupported container")
  | isPrimTyCon constructor = case fixedRep (mkTyConApp constructor (map mkTyVarTy variables)) of
      Just rep | scalarPrimRep rep -> pure (W.ScalarDeclaration rep)
      _ -> pure (opaque W.NominalConstructor "primitive")
  | isAlgTyCon constructor = do
      let constructors = tyConDataCons constructor
      if any (not . isVanillaDataCon) constructors
        then pure (opaque W.NominalConstructor "existential or constrained constructor")
        else if any ((/= length variables) . length . dataConUnivTyVars) constructors
          then lift (Left TypeGraphOriginalDeclarationMismatch)
          else case traverse (traverse (fixedRep . scaledThing) . dataConOrigArgTys) constructors of
            Nothing -> pure (opaque W.NominalConstructor "representation")
            Just representations -> do
              mapM_ (uncurry constructorTemplate) (zip constructors representations)
              pure W.DataDeclaration
  | otherwise = pure (opaque W.NominalConstructor "unnormalized")
 where
  opaque = W.OpaqueDeclaration
  constructorTemplate constructor' representations = do
    let bound = reverse (dataConUnivTyVars constructor')
    fields <- traverse (internExpression bound . scaledThing) (dataConOrigArgTys constructor')
    name <- lift (nameKey (dataConName constructor'))
    family <- lift (declarationKey (dataConTyCon constructor'))
    let physical = map (fixedReps . scaledThing) (dataConRepArgTys constructor')
        marks = map isMarkedStrict (dataConRepStrictness constructor')
        unpacked = map isUnpacked (dataConImplBangs constructor')
        key = TemplateKey name family (dataConTag constructor')
          (tyConFamilySize (dataConTyCon constructor')) physical marks
          (fixedReps (dataConOrigResTy constructor')) unpacked (zip fields representations)
    template <- internExpressionNode key (W.TypeConstructorTemplate constructor')
      [(W.TypeField (fromIntegral index) rep, field)
        | (index, (field, rep)) <- zip [0 :: Int ..] (zip fields representations)]
    addEdge identity (W.TypeConstructor (fromIntegral (dataConTag constructor'))) template
  isUnpacked HsUnpack{} = True
  isUnpacked _ = False

-- Only value kinds with one fixed representation can issue a physical field
-- template. Kind-only syntax remains nominal evidence without invoking the
-- partial GHC value-representation function on an arbitrary kind.
fixedRep :: Type -> Maybe PrimRep
fixedRep ty = case fixedReps ty of Just [rep] -> Just rep; _ -> Nothing

fixedReps :: Type -> Maybe [PrimRep]
fixedReps ty = case sORTKind_maybe (typeKind ty) of
  Just _ -> typePrimRep_maybe ty
  Nothing -> Nothing

scaledThing :: Scaled Type -> Type
scaledThing (Scaled _ ty) = ty

reserveNode :: GraphBuild TypeNodeId
reserveNode = do
  builder <- get
  if tgbNext builder >= 65535 then lift (Left TypeGraphNodeLimit) else pure ()
  chargeWork 1
  let identity = TypeNodeId (tgbNext builder)
  modify' (\current -> current
    { tgbNodes = IntMap.insert (fromIntegral (tgbNext current)) Nothing (tgbNodes current)
    , tgbNext = tgbNext current + 1 })
  pure identity

completeNode :: TypeNodeId -> TypeNodeG -> GraphBuild ()
completeNode (TypeNodeId raw) node = do
  -- This deterministic upper-bound charge includes the primitive CBOR row
  -- overhead, with one aggregate bound across the complete builder.
  let metadata = case node of
        W.TypeRoot _ flags rendered -> length flags + textBytes rendered
        W.TypeDeclaration constructor flags form _ -> length flags + formBytes form + case declarationKey constructor of
          Right (head', _) -> sum (map textBytes [nhUnit head', nhModule head', nhName head'])
          Left _ -> 0
        W.TypeLiteral literal -> case literal of
          W.NaturalTypeLiteral value -> textBytes value
          W.SymbolTypeLiteral value -> textBytes value
          W.CharacterTypeLiteral _ -> 4
        _ -> 0
  chargeBytes (32 + metadata)
  modify' (\builder -> builder { tgbNodes = IntMap.insert (fromIntegral raw) (Just node) (tgbNodes builder) })

addEdge :: TypeNodeId -> W.TypeEdgeRoleF PrimRep -> TypeNodeId -> GraphBuild ()
addEdge source role target = do
  builder <- get
  if tgbEdgeCount builder >= 16777216 then lift (Left TypeGraphEdgeLimit) else pure ()
  chargeWork 1
  chargeBytes 32
  modify' (\current -> current
    { tgbEdges = IntMap.insertWith (++) (nodeIndex source) [(role, target)] (tgbEdges current)
    , tgbEdgeCount = tgbEdgeCount current + 1 })

nodeIndex :: TypeNodeId -> Int
nodeIndex (TypeNodeId raw) = fromIntegral raw

chargeWork :: Int -> GraphBuild ()
chargeWork amount = do
  work <- gets ((+ amount) . tgbWork)
  if work > 16777216 then lift (Left TypeGraphWorkLimit)
    else modify' (\builder -> builder { tgbWork = work })

chargeBytes :: Int -> GraphBuild ()
chargeBytes amount = do
  chargeWork amount
  bytes <- gets ((+ amount) . tgbBytes)
  if bytes > 16777216 then lift (Left TypeGraphByteLimit)
    else modify' (\builder -> builder { tgbBytes = bytes })

formBytes :: W.DeclarationFormF PrimRep -> Int
formBytes (W.OpaqueDeclaration _ reason) = textBytes reason
formBytes _ = 0

textBytes :: Text -> Int
textBytes = BS.length . TextEncoding.encodeUtf8

scalarPrimRep :: PrimRep -> Bool
scalarPrimRep rep = case rep of
  IntRep -> True
  WordRep -> True
  Int8Rep -> True
  Word8Rep -> True
  Int16Rep -> True
  Word16Rep -> True
  Int32Rep -> True
  Word32Rep -> True
  Int64Rep -> True
  Word64Rep -> True
  FloatRep -> True
  DoubleRep -> True
  _ -> False

isSpecial :: String -> String -> TyCon -> Bool
isSpecial owner occurrence tc = definedIn owner tc
  && occNameString (nameOccName (tyConName tc)) == occurrence

isForbidden :: TyCon -> Bool
isForbidden tc = any (\(owner, occurrence) -> isSpecial owner occurrence tc)
  [ ("Data.Map.Internal", "Map")
  , ("Data.Set.Internal", "Set")
  , ("Tidepool.Internal.ExitCell", "ExitCell")
  ]

definedIn :: String -> TyCon -> Bool
definedIn expected tc = maybe False
  ((== expected) . moduleNameString . moduleName)
  (nameModule_maybe (tyConName tc))

renderType :: Type -> Text
renderType = T.pack . renderWithContext stableContext . ppr
  where
    stableContext = defaultSDocContext { sdocSuppressUniques = True }

-- | Whether a name belongs to the @ghc@ compiler package rather than a
-- runtime package such as @ghc-prim@ or @ghc-internal@. Compiler API values
-- can enter Core through Template Haskell helpers, but cannot be executed by
-- Tidepool's runtime.
isGhcCompilerName :: Name -> Bool
isGhcCompilerName name = case nameModule_maybe name of
  Just m -> case List.stripPrefix "ghc-" (unitIdString (moduleUnitId m)) of
    Just (c : _) -> isDigit c
    _            -> False
  Nothing -> False

isGhcCompilerTyCon :: TyCon -> Bool
isGhcCompilerTyCon = isGhcCompilerName . tyConName

-- | Defining modules required to name a type in generated source.
--
-- The raw head is included alongside GHC's synonym-expanding traversal. This
-- preserves the module of a user-written type synonym while still discovering
-- the modules of types nested beneath it. Results are stable and deduplicated.
modulesOfType :: Type -> [Text]
modulesOfType = List.nub . map nhModule . nominalHeadsOfType

nominalHeadsOfType :: Type -> [NominalHead]
nominalHeadsOfType ty = List.sort . List.nub $ headTyCon ty ++
  mapMaybe nominal (USet.nonDetEltsUniqSet (tyConsOfType ty))
  where
    headTyCon (TyConApp tc _) = maybeToList (nominal tc)
    headTyCon _ = []
    nominal tc = do
      m <- nameModule_maybe (tyConName tc)
      pure NominalHead
        { nhUnit = T.pack (unitIdString (moduleUnitId m))
        , nhModule = T.pack (moduleNameString (moduleName m))
        , nhName = T.pack (occNameString (nameOccName (tyConName tc)))
        }

-- | The exact nominal type constructor at a value boundary. Unlike
-- 'nominalHeadsOfType', this never descends into arguments: a @Text@ nested
-- in @Job Text@ is not evidence that the value itself is @Text@.
rootNominalHeadOfType :: Type -> Maybe NominalHead
rootNominalHeadOfType ty = go body
  where
    (_, _, body) = tcSplitSigmaTy ty
    go candidate = case coreView candidate of
      Just expanded -> go expanded
      Nothing -> case candidate of
        CastTy inner _ -> go inner
        _ -> case splitTyConApp_maybe candidate of
          Just (tc, _) -> do
            m <- nameModule_maybe (tyConName tc)
            pure NominalHead
              { nhUnit = T.pack (unitIdString (moduleUnitId m))
              , nhModule = T.pack (moduleNameString (moduleName m))
              , nhName = T.pack (occNameString (nameOccName (tyConName tc)))
              }
          Nothing -> Nothing

-- | Replace effect-row aliases with their exact underlying @Eff '[...]@ type.
--
-- Per-incarnation aliases such as @M@ are convenient authored syntax but are
-- the wrong persisted contract: a later compilation could resolve the same
-- spelling to a different row. We therefore expand a synonym only when its
-- fully expanded meaning contains freer-simple's @Eff@. Ordinary domain
-- aliases remain intact, while aliases nested inside functions and records'
-- type arguments are handled recursively.
--
-- This is normalization, not a prohibition. Effectful functions and values
-- may cross a session or typed-suspension boundary; GHC checks their exact row
-- when the receiving program uses them.
stabilizeEffectRows :: Type -> Type
stabilizeEffectRows = go
  where
    go ty@(TyConApp tc args)
      | isTypeSynonymTyCon tc
      , containsEff (expandTypeSynonyms ty)
      , Just expanded <- coreView ty
      = go expanded
      | otherwise = TyConApp tc (map go args)
    go (AppTy f x) = AppTy (go f) (go x)
    go (ForAllTy binder body) = ForAllTy binder (go body)
    go (FunTy flag mult arg result) =
      FunTy flag (go mult) (go arg) (go result)
    go (CastTy ty coercion) = CastTy (go ty) coercion
    go other = other
