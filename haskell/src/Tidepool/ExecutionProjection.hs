module Tidepool.ExecutionProjection
  ( ProjectionContext(..)
  , ProjectionError(..)
  , projectPrepared
  , projectPreparedTarget
  ) where

import Control.Monad (foldM, forM)
import Control.Monad.State.Strict
import Data.Bits (shiftR)
import Data.ByteString qualified as BS
import Data.List (find)
import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Text (Text)
import Data.Text qualified as Text
import Data.Word (Word32, Word64, Word8)
import GHC.Builtin.PrimOps (primOpOcc)
import GHC.Core (AltCon(..))
import GHC.Core.DataCon
  ( DataCon, dataConName, dataConRepArgTys, dataConRepStrictness
  , dataConTyCon, isMarkedStrict )
import GHC.Core.TyCo.Rep (Scaled(..), Type)
import GHC.Core.TyCon qualified as GHC
import GHC.Core.Type (splitFunTys)
import GHC.Float (castDoubleToWord64, castFloatToWord32)
import GHC.Stg.Syntax
import GHC.Stg.Syntax qualified as Stg
import GHC.Types.Literal (LitNumType(..), Literal(..), literalType)
import GHC.Types.Name (Name, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.RepType (typePrimRep_maybe)
import GHC.Types.Var (Id, varName, varType)
import GHC.Types.Var.Set (dVarSetElems)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import Tidepool.ExecutionSchema
import Tidepool.ExecutionSchema qualified as Schema
import Tidepool.PreparedStg (PreparedModule(..))

data ProjectionContext = ProjectionContext
  { projectionProfile :: Text
  , projectionToolchain :: Text
  , projectionTarget :: TargetDescriptor
  , projectionRetainedGenerations :: Map SymbolIdentity Word64
  , projectionEntry :: SymbolIdentity
  } deriving stock (Eq, Show)

data ProjectionError
  = UnsupportedPreparedShape Text
  | InvalidPreparedIdentity Text
  | InvalidPreparedRepresentation Text
  | InvalidPreparedLayout Text
  | MissingPreparedEntry SymbolIdentity
  deriving stock (Eq, Show)

data PState = PState
  { nextValue :: Word32, nextJoin :: Word32
  , values :: [(Id, ValueId)], joins :: [(Id, JoinId)]
  , globals :: [(Id, GlobalId)], globalDecls :: [GlobalDecl]
  , constructors :: [(DataCon, ConstructorId)], constructorDecls :: [ConstructorDecl]
  , operations :: [(Text, OperationId)], operationDecls :: [OperationDecl]
  , signatures :: [(Signature, SignatureId)]
  , target :: TargetDescriptor
  , retainedGenerations :: Map SymbolIdentity Word64
  }

type P a = StateT PState (Either ProjectionError) a

projectPrepared :: ProjectionContext -> [PreparedModule] -> Either ProjectionError WireProgram
projectPrepared _ [] = Left (UnsupportedPreparedShape "execution program has no modules")
projectPrepared context modules = do
  let initial = PState 0 0 [] [] [] [] [] [] [] [] []
        (projectionTarget context) (projectionRetainedGenerations context)
  (bindingGroups, final) <- runStateT (preallocate modules >> concat <$> mapM projectModule modules) initial
  entry <- maybe (Left (MissingPreparedEntry (projectionEntry context)))
    (pure . topValue) (findTop (projectionEntry context) bindingGroups)
  pure WireProgram
    { programEnvelope = ProgramEnvelope schemaVersion (projectionProfile context)
        (projectionToolchain context) executionAbiVersion (projectionTarget context)
    , programSignatures = map fst (signatures final)
    , programGlobals = globalDecls final
    , programConstructors = constructorDecls final
    , programOperations = operationDecls final
    , programBindings = bindingGroups
    , programEntry = entry
    }
  where
    topValue (TopBinding _ binding) = heapBindingId binding
    findTop wanted = foldr (findGroup wanted) Nothing
    findGroup wanted group found = case filter ((== wanted) . topSymbol) (groupItems group) of
      top : _ -> Just top
      [] -> found
    groupItems (NonRecursive top) = [top]
    groupItems (Recursive tops) = tops
    topSymbol (TopBinding symbol _) = symbol

-- | Project only the home-module closure reachable from the selected entry.
-- Package imports remain explicit globals for atomic linking. This avoids
-- rejecting unrelated polymorphic bindings while retaining every local
-- dependency of the entry.
projectPreparedTarget :: ProjectionContext -> [PreparedModule] -> Either ProjectionError WireProgram
projectPreparedTarget context modules =
  projectPrepared context
    [ prepared { pmBindings = filter isReachable (pmBindings prepared) }
    | prepared <- modules
    , any isReachable (pmBindings prepared)
    ]
  where
    allBindings = concatMap pmBindings modules
    entry = projectionEntry context
    seedIds =
      [ binder
      | (binding, _) <- allBindings
      , binder <- topBinders binding
      , idSymbol "value" binder == entry
      ]
    reachableIds = close seedIds
    isReachable (binding, _) = any (`elem` reachableIds) (topBinders binding)
    close needed =
      let dependencies =
            [ dependency
            | (binding, freeVars) <- allBindings
            , any (`elem` needed) (topBinders binding)
            , dependency <- nonDetEltsUniqSet freeVars
            ]
          expanded = foldr (\binder acc -> if binder `elem` acc then acc else binder : acc)
            [] (needed ++ dependencies)
      in if length expanded == length needed then needed else close expanded

topBinders :: CgStgTopBinding -> [Id]
topBinders (StgTopStringLit binder _) = [binder]
topBinders (StgTopLifted binding) = bindingBinders binding

preallocate :: [PreparedModule] -> P ()
preallocate = mapM_ (mapM_ allocateTop . pmBindings)
  where
    allocateTop (StgTopStringLit binder _, _) = ensureValue binder >> pure ()
    allocateTop (StgTopLifted binding, _) = mapM_ ensureValue (bindingBinders binding) >> pure ()

projectModule :: PreparedModule -> P [Group TopBinding]
projectModule = mapM (projectTop . fst) . pmBindings

projectTop :: CgStgTopBinding -> P (Group TopBinding)
projectTop (StgTopStringLit binder bytes) = do
  identity <- requireValue binder
  signature <- internSignature =<< signatureForType (varType binder)
  pure (NonRecursive (TopBinding (idSymbol "value" binder)
    (HeapBinding identity (Thunk signature Memoize [] (Return [Scalar (BytesLiteral bytes)])))))
projectTop (StgTopLifted (StgNonRec binder rhs)) = NonRecursive <$> projectTopPair binder rhs
projectTop (StgTopLifted (StgRec pairs)) = Recursive <$> mapM (uncurry projectTopPair) pairs

projectTopPair :: Id -> CgStgRhs -> P TopBinding
projectTopPair binder rhs = TopBinding (idSymbol "value" binder)
  <$> (HeapBinding <$> requireValue binder <*> projectRhs binder rhs)

projectRhs :: Id -> CgStgRhs -> P HeapRhs
projectRhs binder (StgRhsClosure captures _ update parameters body resultType) = do
  captureRefs <- mapM projectReference (dVarSetElems captures)
  parameterIds <- mapM ensureValue parameters
  projectedBody <- projectExpr body
  case update of
    ReEntrant -> Function <$> (internSignature =<< signatureFor parameters resultType)
      <*> pure parameterIds <*> pure captureRefs <*> pure projectedBody
    Updatable -> do
      signature <- internSignature . Signature [] =<< repsForType resultType
      pure (Thunk signature Memoize captureRefs projectedBody)
    Stg.SingleEntry -> do
      signature <- internSignature . Signature [] =<< repsForType resultType
      pure (Thunk signature Schema.SingleEntry captureRefs projectedBody)
    JumpedTo -> failShape ("heap binding marked JumpedTo: " <> symbolText (idSymbol "value" binder))
projectRhs _ (StgRhsCon _ con _ _ args _) = Constructor <$> internConstructor con <*> mapM projectArg args

projectExpr :: CgStgExpr -> P Expr
projectExpr (StgApp function args) = do
  projectedArgs <- mapM projectArg args
  knownJoins <- gets joins
  case lookup function knownJoins of
    Just join -> pure (Jump join projectedArgs)
    Nothing -> do
      callee <- Ref <$> projectReference function
      signature <- internSignature =<< signatureForType (varType function)
      pure (if null args then Enter callee signature else Call callee signature projectedArgs)
projectExpr (StgLit literal) = Return . pure . Scalar <$> projectLiteral literal
projectExpr (StgConApp con _ args _) = Construct <$> internConstructor con <*> mapM projectArg args
projectExpr (StgOpApp op args resultType) = do
  signature <- internSignature =<< signatureForArgs args resultType
  Operation <$> internOperation op signature <*> mapM projectArg args
projectExpr (StgCase scrutinee binder _ alts) = Case <$> projectExpr scrutinee
  <*> ensureValue binder <*> repsForType (varType binder) <*> mapM projectAlt alts
projectExpr (StgLet _ binding body) = do
  mapM_ ensureValue (bindingBinders binding)
  Let <$> projectLocalGroup binding <*> projectExpr body
projectExpr (StgLetNoEscape _ binding body) = do
  mapM_ ensureJoin (bindingBinders binding)
  LetJoins <$> projectJoinGroup binding <*> projectExpr body
projectExpr (StgTick _ body) = projectExpr body

projectAlt :: CgStgAlt -> P Alternative
projectAlt (GenStgAlt con binders body) = Alternative <$> projectPattern con
  <*> mapM ensureValue binders <*> projectExpr body

projectPattern :: AltCon -> P AlternativePattern
projectPattern DEFAULT = pure DefaultPattern
projectPattern (DataAlt con) = ConstructorPattern <$> internConstructor con
projectPattern (LitAlt literal) = LiteralPattern <$> projectLiteral literal

projectLocalGroup :: CgStgBinding -> P (Group HeapBinding)
projectLocalGroup (StgNonRec binder rhs) = NonRecursive
  <$> (HeapBinding <$> requireValue binder <*> projectRhs binder rhs)
projectLocalGroup (StgRec pairs) = Recursive <$> forM pairs (\(binder, rhs) ->
  HeapBinding <$> requireValue binder <*> projectRhs binder rhs)

projectJoinGroup :: CgStgBinding -> P (Group JoinBinding)
projectJoinGroup (StgNonRec binder rhs) = NonRecursive <$> projectJoin binder rhs
projectJoinGroup (StgRec pairs) = Recursive <$> mapM (uncurry projectJoin) pairs

projectJoin :: Id -> CgStgRhs -> P JoinBinding
projectJoin binder (StgRhsClosure _ _ JumpedTo parameters body resultType) = JoinBinding
  <$> requireJoin binder
  <*> (internSignature =<< signatureFor parameters resultType)
  <*> mapM ensureValue parameters
  <*> projectExpr body
projectJoin binder _ = failShape
  ("let-no-escape binding lacks JumpedTo form: " <> symbolText (idSymbol "join" binder))

projectArg :: StgArg -> P Atom
projectArg (StgVarArg binder) = Ref <$> projectReference binder
projectArg (StgLitArg literal) = Scalar <$> projectLiteral literal

projectReference :: Id -> P ValueRef
projectReference binder = do
  known <- gets values
  maybe (Global <$> internGlobal binder) (pure . Local) (lookup binder known)

bindingBinders :: CgStgBinding -> [Id]
bindingBinders (StgNonRec binder _) = [binder]
bindingBinders (StgRec pairs) = map fst pairs

ensureValue :: Id -> P ValueId
ensureValue binder = do
  known <- gets values
  case lookup binder known of
    Just identity -> pure identity
    Nothing -> do
      identity <- ValueId <$> gets nextValue
      modify' (\current -> current { nextValue = nextValue current + 1, values = (binder, identity) : values current })
      pure identity

requireValue :: Id -> P ValueId
requireValue binder = gets (lookup binder . values) >>= maybe
  (failIdentity ("missing value allocation: " <> symbolText (idSymbol "value" binder))) pure

ensureJoin :: Id -> P JoinId
ensureJoin binder = do
  known <- gets joins
  case lookup binder known of
    Just identity -> pure identity
    Nothing -> do
      identity <- JoinId <$> gets nextJoin
      modify' (\current -> current { nextJoin = nextJoin current + 1, joins = (binder, identity) : joins current })
      pure identity

requireJoin :: Id -> P JoinId
requireJoin binder = gets (lookup binder . joins) >>= maybe
  (failIdentity ("missing join allocation: " <> symbolText (idSymbol "join" binder))) pure

internGlobal :: Id -> P GlobalId
internGlobal binder = do
  known <- gets globals
  case lookup binder known of
    Just identity -> pure identity
    Nothing -> do
      signature <- internSignature =<< signatureForType (varType binder)
      existing <- gets globalDecls
      generations <- gets retainedGenerations
      let identity = GlobalId (fromIntegral (length existing))
          symbol = idSymbol "value" binder
          declaration = GlobalDecl symbol signature False (Map.lookup symbol generations)
      modify' (\current -> current
        { globals = globals current <> [(binder, identity)]
        , globalDecls = globalDecls current <> [declaration] })
      pure identity

internSignature :: Signature -> P SignatureId
internSignature signature = do
  known <- gets signatures
  case find ((== signature) . fst) known of
    Just (_, identity) -> pure identity
    Nothing -> do
      let identity = SignatureId (fromIntegral (length known))
      modify' (\current -> current { signatures = signatures current <> [(signature, identity)] })
      pure identity

internConstructor :: DataCon -> P ConstructorId
internConstructor con = do
  known <- gets constructors
  case lookup con known of
    Just identity -> pure identity
    Nothing -> do
      reps <- concat <$> mapM (repsForType . scaledThing) (dataConRepArgTys con)
      layout <- layoutFor reps
      prior <- gets constructorDecls
      let identity = ConstructorId (fromIntegral (length prior))
          sourceStrictness = map isMarkedStrict (dataConRepStrictness con) <> repeat False
          fieldStrictness = zipWith (\rep marked -> marked || isUnboxed rep) reps sourceStrictness
          declaration = ConstructorDecl
            (nameSymbol "constructor" (dataConName con))
            (nameSymbol "type" (GHC.tyConName (dataConTyCon con)))
            reps fieldStrictness layout
      modify' (\current -> current
        { constructors = constructors current <> [(con, identity)]
        , constructorDecls = constructorDecls current <> [declaration] })
      pure identity
  where
    scaledThing (Scaled _ ty) = ty
    isUnboxed LiftedRefRep = False
    isUnboxed UnliftedRefRep = False
    isUnboxed _ = True

internOperation :: StgOp -> SignatureId -> P OperationId
internOperation op signature = case op of
  StgPrimOp primop -> do
      let operationName = Text.pack (occNameString (primOpOcc primop))
      known <- gets operations
      case lookup operationName known of
       Just identity -> pure identity
       Nothing -> do
        prior <- gets operationDecls
        let identity = OperationId (fromIntegral (length prior))
            declaration = OperationDecl operationName signature
        modify' (\current -> current
          { operations = operations current <> [(operationName, identity)]
          , operationDecls = operationDecls current <> [declaration] })
        pure identity
  _ -> failShape "foreign/prim-call operation lacks a structured operation contract"

signatureFor :: [Id] -> Type -> P Signature
signatureFor args result = Signature <$> (concat <$> mapM (repsForType . varType) args) <*> repsForType result

signatureForType :: Type -> P Signature
signatureForType ty = do
  let (arguments, result) = splitFunTys ty
  Signature <$> (concat <$> mapM (repsForType . scaledThing) arguments) <*> repsForType result
  where scaledThing (Scaled _ argument) = argument

signatureForArgs :: [StgArg] -> Type -> P Signature
signatureForArgs args result = Signature <$> (concat <$> mapM argReps args) <*> repsForType result
  where
    argReps (StgVarArg binder) = repsForType (varType binder)
    argReps (StgLitArg literal) = repsForType (literalType literal)

repsForType :: Type -> P [RuntimeRep]
repsForType ty = maybe (failRepresentation "runtime-polymorphic representation")
  (mapM projectRep) (typePrimRep_maybe ty)

projectRep :: GHC.PrimRep -> P RuntimeRep
projectRep (GHC.BoxedRep (Just GHC.Lifted)) = pure LiftedRefRep
projectRep (GHC.BoxedRep (Just GHC.Unlifted)) = pure UnliftedRefRep
projectRep (GHC.BoxedRep Nothing) = failRepresentation "runtime-polymorphic boxed representation"
projectRep GHC.AddrRep = pure AddressRep
projectRep GHC.IntRep = targetWidth IntRep
projectRep GHC.WordRep = targetWidth WordRep
projectRep GHC.Int8Rep = pure (IntRep 8)
projectRep GHC.Word8Rep = pure (WordRep 8)
projectRep GHC.Int16Rep = pure (IntRep 16)
projectRep GHC.Word16Rep = pure (WordRep 16)
projectRep GHC.Int32Rep = pure (IntRep 32)
projectRep GHC.Word32Rep = pure (WordRep 32)
projectRep GHC.Int64Rep = pure (IntRep 64)
projectRep GHC.Word64Rep = pure (WordRep 64)
projectRep GHC.FloatRep = pure (FloatRep 32)
projectRep GHC.DoubleRep = pure (FloatRep 64)
projectRep GHC.VecRep{} = failRepresentation "vector representation"

targetWidth :: (Word8 -> RuntimeRep) -> P RuntimeRep
targetWidth constructor = constructor . targetWordWidth <$> gets target

layoutFor :: [RuntimeRep] -> P CheckedLayout
layoutFor reps = do
  machine <- gets target
  let stored = filter (/= VoidRep) reps
  (fields, end, alignment) <- foldM (place machine) ([], 0, 1) stored
  pure (CheckedLayout fields alignment (alignUp end alignment) (map isRoot stored))
  where
    place machine (fields, cursor, greatest) rep = do
      size <- repBytes machine rep
      let alignment = max 1 size; offset = alignUp cursor alignment
      pure (fields <> [FieldLayout rep offset], offset + size, max greatest alignment)
    isRoot LiftedRefRep = True
    isRoot UnliftedRefRep = True
    isRoot _ = False

repBytes :: TargetDescriptor -> RuntimeRep -> P Word32
repBytes machine rep = width $ case rep of
  VoidRep -> 0
  LiftedRefRep -> targetPointerWidth machine
  UnliftedRefRep -> targetPointerWidth machine
  AddressRep -> targetPointerWidth machine
  IntRep bits -> bits
  WordRep bits -> bits
  FloatRep bits -> bits
  where
    width 0 = pure 0
    width bits | bits `mod` 8 == 0 = pure (fromIntegral bits `div` 8)
    width _ = failLayout "non-byte runtime width"

alignUp :: Word32 -> Word32 -> Word32
alignUp value alignment = ((value + alignment - 1) `div` alignment) * alignment

projectLiteral :: Literal -> P ScalarLiteral
projectLiteral literal = case literal of
  LitChar character -> pure (CharLiteral (fromIntegral (fromEnum character)))
  LitString bytes -> pure (BytesLiteral bytes)
  LitNumber kind value -> numeric kind value
  LitFloat value -> pure (FloatLiteral 32 (wordBytes 4 (fromIntegral (castFloatToWord32 (fromRational value)))))
  LitDouble value -> pure (FloatLiteral 64 (wordBytes 8 (castDoubleToWord64 (fromRational value))))
  LitNullAddr -> failShape "null address literal has no scalar address encoding"
  LitRubbish{} -> failShape "rubbish literal"
  LitLabel{} -> failShape "relocatable label literal"
  where
    numeric LitNumBigNat _ = failShape "BigNat literal"
    numeric kind value = do
      machine <- gets target
      let (signed, bits) = case kind of
            LitNumInt -> (True, targetWordWidth machine)
            LitNumInt8 -> (True, 8); LitNumInt16 -> (True, 16)
            LitNumInt32 -> (True, 32); LitNumInt64 -> (True, 64)
            LitNumWord -> (False, targetWordWidth machine)
            LitNumWord8 -> (False, 8); LitNumWord16 -> (False, 16)
            LitNumWord32 -> (False, 32); LitNumWord64 -> (False, 64)
          bytes = integerBytes bits value
      pure (if signed then IntLiteral bits bytes else WordLiteral bits bytes)

integerBytes :: Word8 -> Integer -> BS.ByteString
integerBytes bits value = BS.pack
  [ fromIntegral (normalized `shiftR` (byte * 8))
  | byte <- reverse [0 .. fromIntegral bits `div` 8 - 1] ]
  where normalized = value `mod` (2 ^ bits)

wordBytes :: Int -> Word64 -> BS.ByteString
wordBytes count value = BS.pack
  [ fromIntegral (value `shiftR` (byte * 8)) | byte <- reverse [0 .. count - 1] ]

idSymbol :: Text -> Id -> SymbolIdentity
idSymbol namespace = nameSymbol namespace . varName

nameSymbol :: Text -> Name -> SymbolIdentity
nameSymbol namespace name = case nameModule_maybe name of
  Just modul -> SymbolIdentity (Text.pack (unitString (moduleUnit modul)))
    (Text.pack (moduleNameString (moduleName modul))) namespace
    (Text.pack (occNameString (nameOccName name)))
  Nothing -> SymbolIdentity "<interactive>" "<local>" namespace
    (Text.pack (occNameString (nameOccName name)))

symbolText :: SymbolIdentity -> Text
symbolText symbol = symbolUnit symbol <> ":" <> symbolModule symbol <> ":"
  <> symbolNamespace symbol <> ":" <> symbolOccurrence symbol

failShape :: Text -> P a
failShape = lift . Left . UnsupportedPreparedShape
failIdentity :: Text -> P a
failIdentity = lift . Left . InvalidPreparedIdentity
failRepresentation :: Text -> P a
failRepresentation = lift . Left . InvalidPreparedRepresentation
failLayout :: Text -> P a
failLayout = lift . Left . InvalidPreparedLayout
