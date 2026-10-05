-- | The existing bounded closed activation type encoder. Reply evidence uses
-- its finite declaration graph; this expression encoding owns activation seals.
module Tidepool.CanonicalTypeShape
  ( CanonicalTypeShape, TypeShapeError(..)
  , captureClosedTypeShape, canonicalShapeExpressionBytes
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
import GHC.Core.Type (coreView)
import GHC.Core.TyCo.Rep (Type(..), TyLit(..))
import GHC.Core.TyCon (isFamilyTyCon, tyConName)
import GHC.Data.FastString (unpackFS)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (isDataOcc, occNameString)
import GHC.Types.Var (VarBndr(..), ForAllTyFlag(..), Specificity(..), FunTyFlag(..), isTyVar, varType)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)

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

data CanonicalTypeShape = CanonicalTypeShape
  { canonicalShapeExpressionBytes :: BS.ByteString
  , canonicalShapeOwners :: [Module]
  , canonicalShapeNodeCount :: Int
  }

instance Eq CanonicalTypeShape where
  first == second = canonicalShapeExpressionBytes first == canonicalShapeExpressionBytes second

instance Show CanonicalTypeShape where
  show value = "CanonicalTypeShape " ++ show (canonicalShapeNodeCount value)

-- Existing activation bytes are precisely the unwrapped closed expression.
-- This profile continues refusing families, casts, coercions and free vars.
captureClosedTypeShape :: Type -> Either TypeShapeError CanonicalTypeShape
captureClosedTypeShape original = do
  ((expression, owners), count) <- runStateT (shape 0 [] original) (0 :: Int)
  let expressionBytes = toStrictByteString expression
  if BS.length expressionBytes > 4 * 1024 * 1024
    then Left TypeShapeByteLimit
    else Right (CanonicalTypeShape expressionBytes
      (Map.elems (Map.fromList [(ownerIdentity owner, owner) | owner <- owners])) count)
 where
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
          | isFamilyTyCon constructor -> lift (Left TypeShapeUnresolvedFamily)
          | Just owner <- nameModule_maybe (tyConName constructor) -> do
              children <- traverse (shape (depth + 1) bound) arguments
              let name = tyConName constructor
                  namespace = if isDataOcc (nameOccName name) then "data" else "type"
              pure (encodeListLen 3 <> text "con"
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

