-- | One bounded nominal/de-Bruijn type structure issuer. Activation sealing
-- remains in CheckedCell; these structures confer no runtime authority.
module Tidepool.CanonicalTypeShape
  ( CanonicalTypeShape, TypeShapeScope, TypeShapeError(..)
  , closedTypeShapeScope, constructorTypeShapeScope
  , captureClosedTypeShape, captureGraphTypeShape
  , canonicalShapeExpressionBytes, canonicalShapeIdentityBytes
  , canonicalShapeOwners, canonicalShapeNodeCount
  ) where

import Codec.CBOR.Encoding (Encoding, encodeListLen, encodeString, encodeInt)
import Codec.CBOR.Write (toStrictByteString)
import Control.Monad.State.Strict (StateT, runStateT, get, put, lift)
import qualified Data.ByteString as BS
import Data.List (elemIndex)
import qualified Data.Map.Strict as Map
import qualified Data.Text as T
import GHC (Type, TyVar, Module)
import GHC.Core.DataCon (DataCon, dataConUserTyVarBinders)
import GHC.Core.Type (coreView)
import GHC.Core.TyCo.Rep (Type(..), TyLit(..))
import GHC.Core.TyCon (isFamilyTyCon, tyConName)
import GHC.Data.FastString (unpackFS)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (isDataOcc, occNameString)
import GHC.Types.Var (VarBndr(..), ForAllTyFlag(..), Specificity(..), FunTyFlag(..), isTyVar, varType)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)

-- Scope is captured from the original source DataCon, not an instantiated
-- request or the worker's ghost equality variables.
data TypeShapeScope = ClosedScope | ConstructorScope [(TyVar, Specificity)]

data TypeShapeError
  = TypeShapeDepthLimit
  | TypeShapeNodeLimit
  | TypeShapeByteLimit
  | TypeShapeFreeVariable
  | TypeShapeUnresolvedFamily
  | TypeShapeLocalName
  | TypeShapeCoercionBinder
  | TypeShapeCast
  | TypeShapeCoercion
  deriving (Eq, Ord, Show)

closedTypeShapeScope :: TypeShapeScope
closedTypeShapeScope = ClosedScope

constructorTypeShapeScope :: DataCon -> TypeShapeScope
constructorTypeShapeScope constructor = ConstructorScope
  [(variable, specificity) | Bndr variable specificity <- dataConUserTyVarBinders constructor]

data CanonicalTypeShape = CanonicalTypeShape
  { canonicalShapeExpressionBytes :: BS.ByteString
  , canonicalShapeIdentityBytes :: BS.ByteString
  , canonicalShapeOwners :: [Module]
  , canonicalShapeNodeCount :: Int
  }

instance Eq CanonicalTypeShape where
  first == second = canonicalShapeIdentityBytes first == canonicalShapeIdentityBytes second

instance Show CanonicalTypeShape where
  show value = "CanonicalTypeShape " ++ show (canonicalShapeNodeCount value)

-- Existing activation bytes are precisely the unwrapped closed expression.
-- This profile continues refusing families, casts, coercions and free vars.
captureClosedTypeShape :: Type -> Either TypeShapeError CanonicalTypeShape
captureClosedTypeShape = captureShape False ClosedScope

captureGraphTypeShape :: TypeShapeScope -> Type -> Either TypeShapeError CanonicalTypeShape
captureGraphTypeShape = captureShape True

captureShape :: Bool -> TypeShapeScope -> Type -> Either TypeShapeError CanonicalTypeShape
captureShape graphIdentity scope original = do
  ((expression, identity, owners), count) <- runCapture
  let expressionBytes = toStrictByteString expression
      identityBytes = toStrictByteString identity
  if BS.length (if graphIdentity then identityBytes else expressionBytes) > 4 * 1024 * 1024
    then Left TypeShapeByteLimit
    else Right (CanonicalTypeShape expressionBytes identityBytes
      (Map.elems (Map.fromList [(ownerIdentity owner, owner) | owner <- owners])) count)
 where
  runCapture = do
    (result, count) <- runStateT capture (0 :: Int)
    pure (result, count)
  capture = case scope of
    ClosedScope -> do
      (expression, owners) <- shape 0 [] original
      pure (expression, encodeListLen 2 <> encodeInt 0 <> expression, owners)
    ConstructorScope binders -> do
      (prefix, bound, prefixOwners) <- telescope [] binders
      (expression, owners) <- shape 0 bound original
      pure (expression, encodeListLen 3 <> encodeInt 1
        <> encodeListLen (fromIntegral (length binders)) <> prefix <> expression,
        prefixOwners ++ owners)
  telescope bound [] = pure (mempty, bound, [])
  telescope bound ((variable, specificity) : rest)
    | not (isTyVar variable) = lift (Left TypeShapeCoercionBinder)
    | otherwise = do
        (kind, owners) <- shape 0 bound (varType variable)
        (remaining, completed, restOwners) <- telescope (variable : bound) rest
        let visibility = case specificity of SpecifiedSpec -> 1; InferredSpec -> 2
        pure (encodeListLen 2 <> encodeInt visibility <> kind <> remaining,
          completed, owners ++ restOwners)
  ownerIdentity owner = (unitString (moduleUnit owner), moduleNameString (moduleName owner))
  text = encodeString . T.pack
  node :: Int -> Either TypeShapeError ()
  node depth = if depth > 128 then Left TypeShapeDepthLimit else Right ()
  shape :: Int -> [TyVar] -> Type -> StateT Int (Either TypeShapeError) (Encoding, [Module])
  shape depth bound ty = do
    lift (node depth)
    count <- get
    if count >= 65536 then lift (Left TypeShapeNodeLimit) else put (count + 1)
    case coreView ty of
      Just expanded -> shape (depth + 1) bound expanded
      Nothing -> case ty of
        TyVarTy variable -> case elemIndex variable bound of
          Just index | isTyVar variable -> pure (encodeListLen 2 <> text "bound" <> encodeInt index, [])
          _ -> lift (Left TypeShapeFreeVariable)
        TyConApp constructor arguments
          | isFamilyTyCon constructor && not graphIdentity -> lift (Left TypeShapeUnresolvedFamily)
          | Just owner <- nameModule_maybe (tyConName constructor) -> do
              children <- traverse (shape (depth + 1) bound) arguments
              let name = tyConName constructor
                  namespace = if isDataOcc (nameOccName name) then "data" else "type"
              pure (encodeListLen 3 <> text (if isFamilyTyCon constructor then "family" else "con")
                <> encodeListLen 4 <> text (unitString (moduleUnit owner))
                <> text (moduleNameString (moduleName owner)) <> text namespace
                <> text (occNameString (nameOccName name))
                <> encodeListLen (fromIntegral (length children)) <> foldMap fst children,
                owner : concatMap snd children)
          | otherwise -> lift (Left TypeShapeLocalName)
        AppTy function argument -> binary "app" [function, argument]
        FunTy flag multiplicity argument result -> do
          children <- traverse (shape (depth + 1) bound) [multiplicity, argument, result]
          let tag = case flag of FTF_T_T -> 0; FTF_T_C -> 1; FTF_C_T -> 2; FTF_C_C -> 3
          pure (encodeListLen 5 <> text "fun" <> encodeInt tag <> foldMap fst children,
            concatMap snd children)
        ForAllTy (Bndr variable visibility) body
          | isTyVar variable -> do
              (kind, kindOwners) <- shape (depth + 1) bound (varType variable)
              (bodyShape, bodyOwners) <- shape (depth + 1) (variable : bound) body
              let tag = case visibility of Required -> 0; Invisible SpecifiedSpec -> 1; Invisible InferredSpec -> 2
              pure (encodeListLen 4 <> text "forall" <> encodeInt tag <> kind <> bodyShape,
                kindOwners ++ bodyOwners)
          | otherwise -> lift (Left TypeShapeCoercionBinder)
        LitTy literal -> pure (encodeListLen 3 <> text "literal" <> (case literal of
          NumTyLit value -> text "nat" <> text (show value)
          StrTyLit value -> text "symbol" <> text (unpackFS value)
          CharTyLit value -> text "char" <> encodeInt (fromEnum value)), [])
        CastTy{} -> lift (Left TypeShapeCast)
        CoercionTy{} -> lift (Left TypeShapeCoercion)
    where
      binary tag types = do
        children <- traverse (shape (depth + 1) bound) types
        pure (encodeListLen (fromIntegral (1 + length children)) <> text tag <> foldMap fst children,
          concatMap snd children)

