module Tidepool.TypePolicy
  ( isGhcCompilerName
  , isGhcCompilerTyCon
  , modulesOfType
  , nominalHeadsOfType
  , NominalHead(..)
  , TypeNodeId(..)
  , TypeNodeG(..)
  , TypeGraph(..)
  , TypeGraphBuilder
  , emptyTypeGraphBuilder
  , internType
  , finishTypeGraph
  , stabilizeEffectRows
  ) where

import Control.Monad.State.Strict
import Data.Char (isDigit)
import Data.IntMap.Strict (IntMap)
import qualified Data.IntMap.Strict as IntMap
import Data.Text (Text)
import qualified Data.List as List
import Data.Maybe (mapMaybe, maybeToList)
import qualified Data.Text as T
import Data.Word (Word32)
import GHC.Core.DataCon
  ( DataCon, dataConInstOrigArgTys, dataConUnivTyVars, isVanillaDataCon )
import GHC.Core.Map.Type (TypeMap, emptyTypeMap, extendTypeMap, lookupTypeMap)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCo.FVs (tyConsOfType)
import GHC.Core.TyCo.Rep (Scaled(..), Type(..))
import GHC.Core.TyCo.Subst (substTyWith)
import GHC.Core.TyCon
  ( TyCon, isAlgTyCon, isFamilyTyCon, isNewTyCon, isPrimTyCon
  , isTypeSynonymTyCon, newTyConEtadRhs, tyConDataCons, tyConName )
import GHC.Core.Type (coreView, expandTypeSynonyms, mkAppTys, splitTyConApp_maybe)
import GHC.Types.RepType (PrimRep(..), typePrimRep_maybe)
import GHC.Types.Name (Name, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import qualified GHC.Types.Unique.Set as USet
import GHC.Unit.Module (moduleName, moduleNameString)
import GHC.Unit.Types (moduleUnitId, unitIdString)
import GHC.Utils.Outputable (defaultSDocContext, ppr, renderWithContext)

data NominalHead = NominalHead
  { nhUnit :: Text
  , nhModule :: Text
  , nhName :: Text
  }
  deriving (Eq, Ord, Show)

-- | Index into one module's normalized prepared type graph.
newtype TypeNodeId = TypeNodeId Word32
  deriving (Eq, Ord, Show)

-- | GHC-typed evidence retained until projection assigns constructor ids.
-- The original normalized type is retained only for diagnostics/layout
-- refusal; equality authority comes from the graph edges, never its rendering.
data TypeNodeG
  = DataG Type TyCon [TypeNodeId] [(DataCon, [TypeNodeId])]
  | TextG Type [DataCon]
  | IntegerG Type [DataCon]
  | NaturalG Type [DataCon]
  | ScalarG Type PrimRep
  | UnconstructibleG Text Text
  | ProjectionDefectG Text

newtype TypeGraph = TypeGraph { tgNodes :: [TypeNodeG] }

data TypeGraphBuilder = TypeGraphBuilder
  { tgbIndex :: !(TypeMap TypeNodeId)
  , tgbNodes :: !(IntMap TypeNodeG)
  , tgbNext :: !Word32
  , tgbExpansionLimit :: !(Maybe TypeNodeId)
  }

emptyTypeGraphBuilder :: TypeGraphBuilder
emptyTypeGraphBuilder = TypeGraphBuilder emptyTypeMap IntMap.empty 0 Nothing

finishTypeGraph :: TypeGraphBuilder -> TypeGraph
finishTypeGraph builder = TypeGraph
  [ node
  | index <- [0 .. fromIntegral (tgbNext builder) - 1]
  , Just node <- [IntMap.lookup index (tgbNodes builder)]
  ]

-- | Intern a closed GHC type into a bounded cyclic graph. Nodes are reserved
-- before their edges are visited, which makes ordinary recursive algebraic
-- types finite. Expanding recursion is cut off by independent depth/node caps.
internType :: Type -> State TypeGraphBuilder TypeNodeId
internType = internAt 0

internAt :: Int -> Type -> State TypeGraphBuilder TypeNodeId
internAt depth original
  | depth >= 128 = expansionLimitNode
  | otherwise = case normalizeType [] 0 original of
      Left reason -> internRefusal original reason
      Right ty -> do
        builder <- get
        case lookupTypeMap (tgbIndex builder) ty of
          Just node -> pure node
          Nothing
            | tgbNext builder >= 65535 -> expansionLimitNode
            | otherwise -> do
                let node = TypeNodeId (tgbNext builder)
                    placeholder = UnconstructibleG "type expansion limit" (renderType ty)
                put builder
                  { tgbIndex = extendTypeMap (tgbIndex builder) ty node
                  , tgbNodes = IntMap.insert (fromIntegral (tgbNext builder)) placeholder
                      (tgbNodes builder)
                  , tgbNext = tgbNext builder + 1
                  }
                classified <- classifyType depth ty
                modify' (\current -> current
                  { tgbNodes = IntMap.insert (nodeIndex node) classified (tgbNodes current) })
                pure node

classifyType :: Int -> Type -> State TypeGraphBuilder TypeNodeG
classifyType depth ty
  | containsEff ty = pure (unsupported "effectful")
  | otherwise = case ty of
      TyConApp tc args
        | isFamilyTyCon tc -> pure (unsupported "type family")
        | isNewTyCon tc -> pure (unsupported "recursive newtype")
        | isSpecial "Data.Text.Internal" "Text" tc ->
            pure (TextG ty (tyConDataCons tc))
        | isSpecial "GHC.Num.Integer" "Integer" tc ->
            pure (IntegerG ty (tyConDataCons tc))
        | isSpecial "GHC.Num.Natural" "Natural" tc ->
            pure (NaturalG ty (tyConDataCons tc))
        | isForbidden tc -> pure (unsupported "unsupported container")
        | isPrimTyCon tc -> case typePrimRep_maybe ty of
            Just [rep] | scalarPrimRep rep -> pure (ScalarG ty rep)
            _ -> pure (unsupported "primitive")
        | isAlgTyCon tc -> algebraic tc args
        | otherwise -> pure (unsupported "unnormalized")
      FunTy{} -> pure (unsupported "function")
      ForAllTy{} -> pure (unsupported "polymorphic")
      TyVarTy{} -> pure (unsupported "polymorphic")
      CastTy{} -> pure (unsupported "unnormalized")
      CoercionTy{} -> pure (unsupported "unnormalized")
      LitTy{} -> pure (unsupported "unnormalized")
      _ -> pure (unsupported "unnormalized")
 where
  unsupported reason = UnconstructibleG reason (renderType ty)
  algebraic tc args
    | any unsupportedConstructor constructors =
        pure (unsupported "existential or constrained constructor")
    | any ((/= length args) . length . dataConUnivTyVars) constructors =
        pure (ProjectionDefectG "constructor universal argument arity mismatch")
    | otherwise = do
        argumentNodes <- mapM (internAt (depth + 1)) args
        rows <- traverse (constructorRow args) constructors
        pure (DataG ty tc argumentNodes rows)
    where constructors = tyConDataCons tc
  constructorRow args con = do
    fields <- mapM (internAt (depth + 1) . scaledThing)
      (dataConInstOrigArgTys con args)
    pure (con, fields)

expansionLimitNode :: State TypeGraphBuilder TypeNodeId
expansionLimitNode = do
  builder <- get
  case tgbExpansionLimit builder of
    Just node -> pure node
    Nothing -> do
      let node = TypeNodeId (tgbNext builder)
          index = fromIntegral (tgbNext builder)
      put builder
        { tgbNodes = IntMap.insert index
            (UnconstructibleG "type expansion limit" "<type expansion limit>")
            (tgbNodes builder)
        , tgbNext = tgbNext builder + 1
        , tgbExpansionLimit = Just node
        }
      pure node

internRefusal :: Type -> Text -> State TypeGraphBuilder TypeNodeId
internRefusal ty reason = do
  builder <- get
  case lookupTypeMap (tgbIndex builder) ty of
    Just node -> pure node
    Nothing
      | tgbNext builder >= 65535 -> expansionLimitNode
      | otherwise -> do
          let node = TypeNodeId (tgbNext builder)
              index = fromIntegral (tgbNext builder)
          put builder
            { tgbIndex = extendTypeMap (tgbIndex builder) ty node
            , tgbNodes = IntMap.insert index
                (UnconstructibleG reason (renderType ty)) (tgbNodes builder)
            , tgbNext = tgbNext builder + 1
            }
          pure node

normalizeType :: [Type] -> Int -> Type -> Either Text Type
normalizeType active steps ty
  | steps >= 256 = Left "type expansion limit"
  | any (eqType ty) active = Left "recursive newtype"
  | otherwise = case splitTyConApp_maybe ty of
      Just (tc, _)
        | isTypeSynonymTyCon tc
        , Just expanded <- coreView ty
        -> normalizeType (ty : active) (steps + 1) expanded
      Just (tc, args)
        | isNewTyCon tc
        , let (variables, rhs) = newTyConEtadRhs tc
        , length variables <= length args
        -> let (applied, trailing) = splitAt (length variables) args
               represented = mkAppTys (substTyWith variables applied rhs) trailing
           in normalizeType (ty : active) (steps + 1) represented
      _ -> Right ty

unsupportedConstructor :: DataCon -> Bool
unsupportedConstructor = not . isVanillaDataCon

scaledThing :: Scaled Type -> Type
scaledThing (Scaled _ ty) = ty

nodeIndex :: TypeNodeId -> Int
nodeIndex (TypeNodeId value) = fromIntegral value

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

containsEff :: Type -> Bool
containsEff = any (\tc -> isSpecial "Control.Monad.Freer.Internal" "Eff" tc)
  . USet.nonDetEltsUniqSet . tyConsOfType

definedIn :: String -> TyCon -> Bool
definedIn expected tc = maybe False
  ((== expected) . moduleNameString . moduleName)
  (nameModule_maybe (tyConName tc))

renderType :: Type -> Text
renderType = T.pack . renderWithContext defaultSDocContext . ppr

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
