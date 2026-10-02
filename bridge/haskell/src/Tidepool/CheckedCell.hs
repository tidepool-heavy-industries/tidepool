{-# LANGUAGE ScopedTypeVariables #-}

module Tidepool.CheckedCell
  ( CheckedSignature(..), CheckedSignatureName(..)
  , captureCheckedSignature, encodeCheckedSignature
  , CheckedTypeWitness, captureCheckedTypeWitness, sealCheckedTypeWitness
  , encodeCheckedTypeWitness, renderCheckedTypeWitness
  , rewriteCheckedAnnotations
  ) where

import Codec.CBOR.Encoding (Encoding, encodeListLen, encodeString, encodeBytes, encodeInt)
import Codec.CBOR.Write (toStrictByteString)
import Control.Monad.State.Strict (StateT, evalStateT, get, put, lift)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import Tidepool.ExactHydration (OriginalInterfaceArtifacts, originalInterfaceSha256)
import Data.Maybe (catMaybes)
import Numeric (showHex)
import Control.Monad (forM, unless)
import Data.IORef
import Data.List (elemIndex, find, nubBy, sortOn)
import qualified Data.Text as T
import Data.Generics (everywhereM, mkM)
import GHC
import GHC.Core.Type (tyConsOfType, coreView)
import GHC.Core.TyCo.Rep (Type(..), TyLit(..))
import GHC.Data.FastString (unpackFS)
import GHC.Types.Var (VarBndr(..), ForAllTyFlag(..), Specificity(..), FunTyFlag(..), isTyVar)
import Tidepool.TypePolicy (stabilizeEffectRows)
import GHC.Core.TyCon (tyConName, isFamilyTyCon)
import GHC.Driver.Env (lookupType, hsc_home_unit)
import GHC.Driver.Env.Types (hsc_unit_env)
import GHC.Iface.Env (lookupOrig)
import GHC.Iface.Load (importDecl)
import GHC.Tc.Utils.Monad (initIfaceLoad)
import qualified GHC.Data.Maybe as MErr
import GHC.Types.Name (nameModule_maybe, nameOccName, wiredInNameTyThing_maybe)
import GHC.Types.Name.Occurrence
  ( isDataOcc, mkDataOcc, mkTcOcc, occNameString )
import GHC.Types.Name.Ppr (mkNamePprCtx)
import GHC.Types.Name.Reader (RdrName(..), GlobalRdrEnv, emptyGlobalRdrEnv, rdrNameOcc)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Unit.Types (stringToUnit, unitString)
import GHC.Unit.Home (isHomeUnit)
import GHC.Utils.Outputable hiding ((<>), text)

-- The rendering is parser input. Each external type-level Name has a private
-- qualifier, independent of lexical imports or the user's pretty-print scope.
data CheckedSignature = CheckedSignature
  { signatureKey :: String
  , signatureType :: String
  , signatureNames :: [CheckedSignatureName]
  } deriving (Eq, Ord, Show)

data CheckedSignatureName = CheckedSignatureName
  { signatureQualifier :: String
  , signatureUnit :: String
  , signatureModule :: String
  , signatureNamespace :: String
  , signatureOccurrence :: String
  } deriving (Eq, Ord, Show)

captureCheckedSignature :: HscEnv -> String -> Type -> CheckedSignature
captureCheckedSignature env key ty = CheckedSignature key rendered inventory
  where
    originalNames = sortOn identity $ nubBy (==)
      [ tyConName constructor
      | constructor <- nonDetEltsUniqSet (tyConsOfType ty)
      , Just _ <- [nameModule_maybe (tyConName constructor)] ]
    identity :: Name -> (String, String, String, String)
    identity name = case nameModule_maybe name of
      Just owner -> (unitString (moduleUnit owner), moduleNameString (moduleName owner),
        namespace name, occNameString (nameOccName name))
      Nothing -> ("", "", namespace name, occNameString (nameOccName name))
    namespace :: Name -> String
    namespace name = if isDataOcc (nameOccName name) then "data" else "type"
    inventory =
      [ CheckedSignatureName ("TidepoolCheckedName" ++ show index) unit owner nameSpace occurrence
      | (index, name) <- zip [(0 :: Int)..] originalNames
      , let (unit, owner, nameSpace, occurrence) = identity name ]
    names = (mkNamePprCtx (PromTickCtx True True) (hsc_unit_env env) (emptyGlobalRdrEnv :: GlobalRdrEnv))
      { queryQualifyName = \owner occurrence ->
          case find (\entry -> signatureUnit entry == unitString (moduleUnit owner)
              && signatureModule entry == moduleNameString (moduleName owner)
              && signatureOccurrence entry == occNameString occurrence
              && signatureNamespace entry == if isDataOcc occurrence then "data" else "type") inventory of
            Just entry -> NameQual (mkModuleName (signatureQualifier entry))
            Nothing -> NameUnqual }
    rendered = renderWithContext
      (defaultSDocContext { sdocStyle = mkUserStyle names AllTheWay }) (ppr ty)

encodeCheckedSignature :: CheckedSignature -> Encoding
encodeCheckedSignature signature = encodeListLen 3
  <> text (signatureKey signature) <> text (signatureType signature)
  <> encodeListLen (fromIntegral (length (signatureNames signature)))
  <> foldMap (\entry -> encodeListLen 5
      <> text (signatureQualifier entry) <> text (signatureUnit entry)
      <> text (signatureModule entry) <> text (signatureNamespace entry)
      <> text (signatureOccurrence entry)) (signatureNames signature)
  where text = encodeString . T.pack

-- Only the compiler-generated signature binder is rewritten. Authored
-- signatures, expressions and the reader environment retain ordinary lookup.
rewriteCheckedAnnotations
  :: HscEnv -> [(String, CheckedSignature)] -> ParsedModule -> IO ParsedModule
rewriteCheckedAnnotations env annotations parsed = do
  resolved <- forM annotations $ \(binder, signature) -> do
    names <- forM (signatureNames signature) $ \entry -> do
      let owner = mkModule (stringToUnit (signatureUnit entry))
            (mkModuleName (signatureModule entry))
      occurrence <- case signatureNamespace entry of
        "type" -> pure (mkTcOcc (signatureOccurrence entry))
        "data" -> pure (mkDataOcc (signatureOccurrence entry))
        _ -> fail "checked signature has an invalid namespace"
      name <- initIfaceLoad env (lookupOrig owner occurrence)
      found <- case wiredInNameTyThing_maybe name of
        Just thing -> pure (Just thing)
        Nothing -> lookupType env name
      exists <- case found of
        Just _ -> pure True
        Nothing | isHomeUnit (hsc_home_unit env) (moduleUnit owner) -> pure False
        Nothing -> initIfaceLoad env (importDecl name) >>= \loaded -> pure $ case loaded of
          MErr.Succeeded _ -> True
          MErr.Failed _ -> False
      unless exists (fail ("checked signature Name is unavailable in the admitted environment: "
        ++ signatureUnit entry ++ ":" ++ signatureModule entry ++ ":" ++ signatureNamespace entry ++ ":" ++ signatureOccurrence entry))
      pure (entry, name)
    pure (binder, names)
  counts <- newIORef []
  rewritten <- everywhereM (mkM (rewriteSignature counts resolved)) (pm_parsed_source parsed)
  seen <- readIORef counts
  unless (sortOn id seen == sortOn id (map fst annotations))
    (fail "generated checked annotation is missing or duplicated")
  pure parsed { pm_parsed_source = rewritten }
  where
    rewriteSignature :: IORef [String] -> [(String, [(CheckedSignatureName, Name)])] -> Sig GhcPs -> IO (Sig GhcPs)
    rewriteSignature counts resolved signature@(TypeSig extension binders ty) =
      case [(binder,names) | (binder, names) <- resolved,
          map (occNameString . rdrNameOcc . unLoc) binders == [binder]] of
        [] -> pure signature
        [(binder,names)] -> do
          modifyIORef' counts (binder :)
          TypeSig extension binders <$> everywhereM (mkM (rewriteType names)) ty
        _ -> fail "duplicate checked annotation binder"
    rewriteSignature _ _ signature = pure signature

    rewriteType :: [(CheckedSignatureName, Name)] -> HsType GhcPs -> IO (HsType GhcPs)
    rewriteType names ty@(HsTyVar extension promotion located) = case unLoc located of
      Qual qualifier occurrence -> case
          [name | (entry, name) <- names,
            signatureQualifier entry == moduleNameString qualifier,
            signatureOccurrence entry == occNameString occurrence] of
        [name] -> pure (HsTyVar extension promotion (fmap (const (Exact name)) located))
        [] -> fail "generated checked annotation contains an unproved Name"
        _ -> fail "generated checked annotation has ambiguous Name authority"
      _ -> pure ty
    rewriteType _ ty = pure ty


-- The parser rendering is useful presentation, but equality is the complete
-- ordered GHC type structure and the interfaces of its exact original Names.
-- Unsealed witnesses remain transaction-local until product publication.
data CheckedTypeWitness = CheckedTypeWitness
  { witnessSignature :: CheckedSignature
  , witnessStructure :: BS.ByteString
  , witnessOwners :: [Module]
  , witnessInterfaces :: Maybe [(Module, String)]
  } deriving (Eq)

instance Show CheckedTypeWitness where
  show witness = "CheckedTypeWitness " ++ show (witnessSignature witness)

captureCheckedTypeWitness :: HscEnv -> Type -> Maybe CheckedTypeWitness
captureCheckedTypeWitness env original = case evalStateT (shape 0 [] stable) (0 :: Int) of
  Left _ -> Nothing
  Right (encoded, owners) ->
    let bytes = toStrictByteString encoded
    in if BS.length bytes > 4 * 1024 * 1024 then Nothing else Just
      (CheckedTypeWitness (captureCheckedSignature env "activation-input" stable) bytes
        (Map.elems (Map.fromList [(ownerIdentity owner, owner) | owner <- owners])) Nothing)
  where
    stable = stabilizeEffectRows original
    ownerIdentity owner = (unitString (moduleUnit owner), moduleNameString (moduleName owner))
    text = encodeString . T.pack
    node :: Int -> Either String ()
    node depth = if depth > 128 then Left "type witness depth" else Right ()
    shape :: Int -> [TyVar] -> Type -> StateT Int (Either String) (Encoding, [Module])
    shape depth bound ty = do
      lift (node depth)
      count <- get
      if count >= 65536 then lift (Left "type witness node count") else put (count + 1)
      case coreView ty of
        Just expanded -> shape (depth + 1) bound expanded
        Nothing -> case ty of
          TyVarTy variable -> case elemIndex variable bound of
            Just index | isTyVar variable -> pure (encodeListLen 2 <> text "bound" <> encodeInt index, [])
            _ -> lift (Left "type witness free variable")
          TyConApp constructor arguments
            | isFamilyTyCon constructor -> lift (Left "type witness unresolved family")
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
            | otherwise -> lift (Left "type witness local type Name")
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
            | otherwise -> lift (Left "type witness coercion binder")
          LitTy literal -> pure (encodeListLen 3 <> text "literal" <> (case literal of
            NumTyLit value -> text "nat" <> text (show value)
            StrTyLit value -> text "symbol" <> text (unpackFS value)
            CharTyLit value -> text "char" <> encodeInt (fromEnum value)), [])
          CastTy{} -> lift (Left "type witness cast")
          CoercionTy{} -> lift (Left "type witness coercion")
      where
        binary tag types = do
          children <- traverse (shape (depth + 1) bound) types
          pure (encodeListLen (fromIntegral (1 + length children)) <> text tag <> foldMap fst children,
            concatMap snd children)

-- Bind interfaces from this completed transaction's original products and
-- admitted home/package interface environment. Never infer a seal from source.
sealCheckedTypeWitness :: OriginalInterfaceArtifacts -> CheckedTypeWitness
  -> IO (Maybe CheckedTypeWitness)
sealCheckedTypeWitness artifacts witness = do
  seals <- mapM (\owner -> fmap ((,) owner) <$> originalInterfaceSha256 artifacts owner)
    (witnessOwners witness)
  pure $ if length (catMaybes seals) == length seals
    then Just witness { witnessInterfaces = Just (catMaybes seals) } else Nothing

encodeCheckedTypeWitness :: CheckedTypeWitness -> Maybe Encoding
encodeCheckedTypeWitness witness = do
  interfaces <- witnessInterfaces witness
  pure (encodeListLen 5 <> text "TPCANONICALINPUTTYPE1" <> text "1"
    <> encodeCheckedSignature (witnessSignature witness) <> encodeBytes (witnessStructure witness)
    <> encodeListLen (fromIntegral (length interfaces)) <> foldMap seal interfaces)
  where
    text = encodeString . T.pack
    seal (owner, digest) = encodeListLen 3 <> text (unitString (moduleUnit owner))
      <> text (moduleNameString (moduleName owner)) <> text digest

renderCheckedTypeWitness :: CheckedTypeWitness -> Maybe String
renderCheckedTypeWitness witness = hex . toStrictByteString <$> encodeCheckedTypeWitness witness
  where
    hex = concatMap (\byte -> let value = showHex byte "" in if length value == 1 then '0' : value else value) . BS.unpack
