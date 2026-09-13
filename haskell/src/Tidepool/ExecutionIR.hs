-- | A provisional, non-wire inventory of the GHC 9.12 prepared STG handoff.
--
-- This module intentionally records evidence rather than defining Tidepool's
-- eventual execution ABI.  It traverses the structured STG tree and retains
-- names as their defining unit/module/occurrence triple.
module Tidepool.ExecutionIR
  ( ExactName(..)
  , DependencyClass(..)
  , Dependency(..)
  , LiteralInventory(..)
  , RuntimeForm(..)
  , UpdateInventory(..)
  , PreparedFact(..)
  , PreparedSupport(..)
  , PreparedInventory(..)
  , inventoryPreparedModule
  , topBindingReferences
  , renderPreparedInventory
  ) where

import Data.ByteString qualified as BS
import Data.List (intercalate)
import Data.Set (Set)
import Data.Set qualified as Set
import Data.Word (Word8)
import GHC.Core (AltCon(..))
import GHC.Core.DataCon (dataConName, dataConWorkId)
import GHC.Core.TyCon (PrimRep, isUnboxedSumTyCon, isUnboxedTupleTyCon)
import GHC.Core.Type (splitTyConApp_maybe)
import GHC.Core.TyCo.Rep (Type)
import GHC.Data.FastString (unpackFS)
import GHC.Stg.Syntax
import GHC.Builtin.PrimOps (primOpOcc)
import GHC.Types.Literal (LitNumType(..), Literal(..))
import GHC.Types.Name (Name, nameModule_maybe, nameOccName, nameUnique)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Name.Env (nonDetNameEnvElts)
import GHC.Types.RepType (typePrimRep_maybe)
import GHC.Types.Unique (Unique)
import GHC.Types.Unique.Set
  ( UniqSet, addOneToUniqSet, elementOfUniqSet, emptyUniqSet
  , mkUniqSet, unionUniqSets )
import GHC.Types.Var.Set (dVarSetElems)
import GHC.Types.Var (Id, varName, varType, varUnique)
import GHC.Unit.Module (Module, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import Tidepool.PreparedStg (PreparedModule(..))
import Tidepool.PreparedFacts
  ( PreparedFacts(..), PreparedOperation(..), PreparedRepresentation(..)
  , PreparedSupport(..)
  )
import GHC.Utils.Outputable (ppr, showSDocUnsafe)

data ExactName = ExactName
  { exactUnit :: String
  , exactModule :: String
  , exactOccurrence :: String
  } deriving stock (Eq, Ord, Show)

data DependencyClass = LocalDependency | RecursiveDependency
  | GlobalDependency | LibraryDependency
  deriving stock (Eq, Ord, Show)

data Dependency = Dependency DependencyClass ExactName
  deriving stock (Eq, Ord, Show)

data LiteralInventory
  = CharacterLiteral Char
  | NumericLiteral String Integer
  | StringLiteral [Word8] Bool
  | NullAddressLiteral
  | RubbishLiteral
  | FloatLiteral Rational
  | DoubleLiteral Rational
  | LabelLiteral String
  deriving stock (Eq, Ord, Show)

data RuntimeForm
  = RuntimeRepresentation String
  | RuntimePolymorphicRepresentation
  | VoidArgument
  | UnboxedTuple
  | UnboxedSum
  | NonRecursiveBinding
  | RecursiveBinding
  | JoinBinding
  | ClosureRhs
  | ConstructorRhs
  | ApplicationExpr
  | LiteralExpr
  | ConstructorExpr
  | PrimitiveExpr
  | CaseExpr
  | LetExpr
  | TickExpr
  | MultiValueAlternative Int
  | UpdateForm UpdateInventory
  deriving stock (Eq, Ord, Show)

data UpdateInventory = ReEntrantUpdate | UpdatableUpdate
  | SingleEntryUpdate | JumpedToUpdate
  deriving stock (Eq, Ord, Show)

-- | Facts extracted from the production prepared program which later engine
-- stages must either consume or reject explicitly. This remains an evidence
-- inventory, not the M3 wire representation.
data PreparedFact
  = ImportedValue ExactName
  | ClosureCaptures ExactName [ExactName]
  | ConstructorLayout ExactName [String]
  | OperationSignature String [String] [String]
  | TagInferenceCount Int
  | SupportStatus PreparedSupport
  deriving stock (Eq, Ord, Show)

data PreparedInventory = PreparedInventory
  { inventoryModule :: ExactName
  , inventoryDependencies :: Set Dependency
  , inventoryLiterals :: Set LiteralInventory
  , inventoryRuntimeForms :: Set RuntimeForm
  , inventoryFacts :: Set PreparedFact
  } deriving stock (Eq, Show)

data Scope = Scope
  { scopeModule :: Module
  , scopeTopLevel :: UniqSet Unique
  , scopeLocals :: UniqSet Unique
  , scopeRecursive :: UniqSet Unique
  }

data Acc = Acc (Set Dependency) (Set LiteralInventory) (Set RuntimeForm)
  (Set PreparedFact) [(Unique, ExactName)]

instance Semigroup Acc where
  Acc a b c d e <> Acc w x y z q =
    Acc (a <> w) (b <> x) (c <> y) (d <> z) (e <> q)
instance Monoid Acc where
  mempty = Acc mempty mempty mempty mempty mempty

inventoryPreparedModule :: PreparedModule -> PreparedInventory
inventoryPreparedModule prepared =
  let tops = mkUniqSet
        [ varUnique binder
        | (binding, _) <- pmBindings prepared
        , binder <- topIds binding
        ]
      scope = Scope (pmModule prepared) tops emptyUniqSet emptyUniqSet
      Acc dependencies _walkedLiterals forms _walkedFacts _references =
        foldMap (walkTop scope . fst) (pmBindings prepared)
      literals = Set.fromList (map literalInventory (preparedLiterals (pmFacts prepared)))
      imported = Set.fromList
        [ ImportedValue name
        | Dependency LibraryDependency name <- Set.toList dependencies
        ]
      tags = Set.singleton (TagInferenceCount (length (nonDetNameEnvElts (pmTagSigs prepared))))
      facts = inventoryExactFacts (pmModule prepared) (pmFacts prepared)
  in PreparedInventory (moduleIdentity (pmModule prepared)) dependencies literals forms
       (facts <> imported <> tags)

-- | Return top-level references from one binding using the same structured STG
-- walk as the inventory.  The supplied universe is the complete set of
-- candidate top binders across all projected modules; references outside it
-- remain package/import globals and are deliberately omitted.
topBindingReferences :: Module -> UniqSet Unique -> CgStgTopBinding -> UniqSet Unique
topBindingReferences modul topLevel binding =
  let Acc _dependencies _ _ _ references = walkTop scope binding
  in mkUniqSet
    [ unique
    | (unique, _) <- references
    , elementOfUniqSet unique topLevel
    ]
 where
  scope = Scope modul topLevel emptyUniqSet emptyUniqSet

inventoryExactFacts :: Module -> PreparedFacts -> Set PreparedFact
inventoryExactFacts modul facts = Set.fromList
  ( map (ImportedValue . idIdentity modul) (preparedImportedIds facts)
    <> map (\(binder, captures) -> ClosureCaptures (idIdentity modul binder)
         (map (idIdentity modul) captures))
         (preparedClosureCaptures facts)
    <> map constructor (preparedConstructors facts)
    <> map operation (preparedOperations facts)
    <> map SupportStatus (preparedSupport facts)
  )
 where
  constructor (con, fields) = ConstructorLayout
    (idIdentity modul (dataConWorkId con)) (concatMap renderPreparedReps fields)
  operation prepared = OperationSignature
    (operationName (preparedOperation prepared))
    (concatMap renderPreparedReps (preparedArgumentReps prepared))
    (renderPreparedReps (preparedResultReps prepared))

  renderPreparedReps (PreparedKnownReps reps) = map show reps
  renderPreparedReps PreparedRuntimePolymorphic{} = ["RuntimePolymorphic"]

renderPreparedInventory :: PreparedInventory -> String
renderPreparedInventory inventory = intercalate "\n"
  [ "module " <> renderName (inventoryModule inventory)
  , "dependencies " <> renderSet renderDependency (inventoryDependencies inventory)
  , "literals " <> renderSet show (inventoryLiterals inventory)
  , "runtime-forms " <> renderSet show (inventoryRuntimeForms inventory)
  , "prepared-facts " <> renderSet show (inventoryFacts inventory)
  ]

renderSet :: (a -> String) -> Set a -> String
renderSet f = ("[" <>) . (<> "]") . intercalate "," . map f . Set.toAscList

renderName :: ExactName -> String
renderName (ExactName unit modul occurrence) = unit <> ":" <> modul <> ":" <> occurrence

renderDependency :: Dependency -> String
renderDependency (Dependency kind name) = show kind <> "(" <> renderName name <> ")"

moduleIdentity :: Module -> ExactName
moduleIdentity modul = ExactName (unitString (moduleUnit modul))
  (moduleNameString (moduleName modul)) "<module>"

exactName :: Name -> Maybe ExactName
exactName name = do
  modul <- nameModule_maybe name
  pure $ ExactName (unitString (moduleUnit modul))
    (moduleNameString (moduleName modul)) (occNameString (nameOccName name))

idIdentity :: Module -> Id -> ExactName
idIdentity fallback binder = case exactName (varName binder) of
  Just identity -> identity
  Nothing -> ExactName (unitString (moduleUnit fallback))
    (moduleNameString (moduleName fallback)) (occNameString (nameOccName (varName binder)))

topIds :: CgStgTopBinding -> [Id]
topIds (StgTopStringLit binder _) = [binder]
topIds (StgTopLifted binding) = bindingBinders binding

bindingBinders :: CgStgBinding -> [Id]
bindingBinders (StgNonRec binder _) = [binder]
bindingBinders (StgRec pairs) = map fst pairs

walkTop :: Scope -> CgStgTopBinding -> Acc
walkTop _scope (StgTopStringLit binder bytes) =
  binderForms binder <> literalAcc (LitString bytes)
walkTop scope (StgTopLifted binding) = walkBinding scope binding

walkBinding :: Scope -> CgStgBinding -> Acc
walkBinding scope (StgNonRec binder rhs) =
  form NonRecursiveBinding <> binderForms binder <>
  walkRhs (scope { scopeLocals = addId binder (scopeLocals scope) }) rhs
walkBinding scope (StgRec pairs) =
  let recursive = mkUniqSet (map (varUnique . fst) pairs)
      inner = scope { scopeLocals = scopeLocals scope `unionUniqSets` recursive
                    , scopeRecursive = recursive }
  in form RecursiveBinding <> foldMap (\(binder, rhs) -> binderForms binder <> walkRhs inner rhs) pairs

walkRhs :: Scope -> CgStgRhs -> Acc
walkRhs scope (StgRhsClosure captures _ update binders body ty) =
  form ClosureRhs <> form (UpdateForm (updateInventory update)) <> typeForms ty <>
  fact (ClosureCaptures (ExactName "" "" "<unowned>") (map (idIdentity (scopeModule scope))
    (dVarSetElems captures))) <>
  foldMap binderForms binders <>
  walkExpr (scope { scopeLocals = scopeLocals scope `unionUniqSets`
      mkUniqSet (map varUnique binders) }) body
walkRhs scope (StgRhsCon _ con _ _ args ty) =
  form ConstructorRhs <> nameDependency scope (dataConName con) <>
  fact (ConstructorLayout (idIdentity (scopeModule scope) (dataConWorkId con))
    (concatMap argReps args)) <>
  typeForms ty <> foldMap (walkArg scope) args

walkExpr :: Scope -> CgStgExpr -> Acc
walkExpr scope = \case
  StgApp function args -> form ApplicationExpr <> walkId scope function <> foldMap (walkArg scope) args
  StgLit literal -> form LiteralExpr <> literalAcc literal
  -- Tag rewriting may leave the unboxed-sum-only rep annotation undefined
  -- for boxed constructors. Field facts come from the actual arguments.
  StgConApp con _ args _ -> form ConstructorExpr <> nameDependency scope (dataConName con)
    <> fact (ConstructorLayout (idIdentity (scopeModule scope) (dataConWorkId con))
      (concatMap argReps args))
    <> foldMap (walkArg scope) args
  StgOpApp op args ty -> form PrimitiveExpr <> foldMap (walkArg scope) args <> typeForms ty
    <> fact (OperationSignature (operationName op) (concatMap argReps args)
      (renderTypeReps ty))
  StgCase scrutinee binder altType alts -> form CaseExpr <> walkExpr scope scrutinee
    <> binderForms binder <> altTypeForms altType
    <> foldMap (walkAlt (scope { scopeLocals = addId binder (scopeLocals scope) })) alts
  StgLet _ binding body -> form LetExpr <> walkBinding scope binding <> walkExpr (extendBinding scope binding) body
  StgLetNoEscape _ binding body -> form JoinBinding <> walkBinding scope binding <> walkExpr (extendBinding scope binding) body
  StgTick _ body -> form TickExpr <> walkExpr scope body

walkAlt :: Scope -> CgStgAlt -> Acc
walkAlt scope (GenStgAlt con binders rhs) = altConAcc scope con <> foldMap binderForms binders
  <> walkExpr (scope { scopeLocals = scopeLocals scope `unionUniqSets`
      mkUniqSet (map varUnique binders) }) rhs

altConAcc :: Scope -> AltCon -> Acc
altConAcc scope (DataAlt con) = nameDependency scope (dataConName con)
altConAcc _ (LitAlt literal) = literalAcc literal
altConAcc _ DEFAULT = mempty

altTypeForms :: AltType -> Acc
altTypeForms (MultiValAlt n) = form (MultiValueAlternative n)
altTypeForms (AlgAlt tyCon)
  | isUnboxedTupleTyCon tyCon = form UnboxedTuple
  | isUnboxedSumTyCon tyCon = form UnboxedSum
altTypeForms _ = mempty

extendBinding :: Scope -> CgStgBinding -> Scope
extendBinding scope binding = scope
  { scopeLocals = scopeLocals scope `unionUniqSets`
      mkUniqSet (map varUnique (bindingBinders binding)) }

addId :: Id -> UniqSet Unique -> UniqSet Unique
addId binder scope = addOneToUniqSet scope (varUnique binder)

walkArg :: Scope -> StgArg -> Acc
walkArg scope (StgVarArg binder) = walkId scope binder <> binderForms binder
walkArg _ (StgLitArg literal) = literalAcc literal

walkId :: Scope -> Id -> Acc
walkId scope binder = dependencyAcc scope (varUnique binder)
  (idIdentity (scopeModule scope) binder) <>
  Acc mempty mempty mempty mempty
    [(varUnique binder, idIdentity (scopeModule scope) binder)]

dependencyAcc :: Scope -> Unique -> ExactName -> Acc
dependencyAcc scope unique name = Acc
  (Set.singleton (Dependency kind name)) mempty mempty mempty mempty
 where
  here = moduleIdentity (scopeModule scope)
  kind
    | elementOfUniqSet unique (scopeRecursive scope) = RecursiveDependency
    | elementOfUniqSet unique (scopeLocals scope) = LocalDependency
    | elementOfUniqSet unique (scopeTopLevel scope) = GlobalDependency
    | exactUnit name == exactUnit here = GlobalDependency
    | otherwise = LibraryDependency

nameDependency :: Scope -> Name -> Acc
nameDependency scope name = maybe mempty
  (dependencyAcc scope (nameUnique name)) (exactName name)

binderForms :: Id -> Acc
binderForms = typeForms . varType

typeForms :: Type -> Acc
typeForms ty = tupleSum <> case typePrimRep_maybe ty of
  Nothing -> form RuntimePolymorphicRepresentation
  Just [] -> form VoidArgument
  Just reps -> foldMap repForm reps
 where
  tupleSum = case splitTyConApp_maybe ty of
    Just (tc, _) | isUnboxedTupleTyCon tc -> form UnboxedTuple
    Just (tc, _) | isUnboxedSumTyCon tc -> form UnboxedSum
    _ -> mempty

repForm :: PrimRep -> Acc
repForm = form . RuntimeRepresentation . show

literalAcc :: Literal -> Acc
literalAcc literal = Acc mempty (Set.singleton (literalInventory literal)) mempty mempty mempty

literalInventory :: Literal -> LiteralInventory
literalInventory literal = case literal of
    LitChar char -> CharacterLiteral char
    LitNumber kind number -> NumericLiteral (litNumName kind) number
    LitString bytes -> let unpacked = map fromIntegral (BS.unpack bytes)
      in StringLiteral unpacked (BS.elem 0 bytes || containsModifiedNul unpacked)
    LitNullAddr -> NullAddressLiteral
    LitRubbish _ _ -> RubbishLiteral
    LitFloat number -> FloatLiteral number
    LitDouble number -> DoubleLiteral number
    LitLabel label _ -> LabelLiteral (unpackFS label)

containsModifiedNul :: [Word8] -> Bool
containsModifiedNul (0xC0 : 0x80 : _) = True
containsModifiedNul (_ : rest) = containsModifiedNul rest
containsModifiedNul [] = False

form :: RuntimeForm -> Acc
form value = Acc mempty mempty (Set.singleton value) mempty mempty

fact :: PreparedFact -> Acc
fact value = Acc mempty mempty mempty (Set.singleton value) mempty

argReps :: StgArg -> [String]
argReps (StgVarArg binder) = renderTypeReps (varType binder)
argReps (StgLitArg literal) = [showSDocUnsafe (ppr literal)]

renderTypeReps :: Type -> [String]
renderTypeReps ty = case typePrimRep_maybe ty of
  Just reps -> map show reps
  Nothing -> ["RuntimePolymorphic"]

operationName :: StgOp -> String
operationName (StgPrimOp op) = occNameString (primOpOcc op)
operationName (StgPrimCallOp call) = showSDocUnsafe (ppr call)
operationName (StgFCallOp call _) = "foreign:" <> showSDocUnsafe (ppr call)

updateInventory :: UpdateFlag -> UpdateInventory
updateInventory ReEntrant = ReEntrantUpdate
updateInventory Updatable = UpdatableUpdate
updateInventory SingleEntry = SingleEntryUpdate
updateInventory JumpedTo = JumpedToUpdate

litNumName :: LitNumType -> String
litNumName LitNumBigNat = "BigNat"
litNumName LitNumInt = "Int"
litNumName LitNumInt8 = "Int8"
litNumName LitNumInt16 = "Int16"
litNumName LitNumInt32 = "Int32"
litNumName LitNumInt64 = "Int64"
litNumName LitNumWord = "Word"
litNumName LitNumWord8 = "Word8"
litNumName LitNumWord16 = "Word16"
litNumName LitNumWord32 = "Word32"
litNumName LitNumWord64 = "Word64"
