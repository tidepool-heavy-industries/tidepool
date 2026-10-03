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
import Control.Monad (forM, forM_, unless)
import Data.IORef
import Data.List (elemIndex, sortOn)
import qualified Data.Text as T
import Data.Generics (everywhereM, mkM)
import GHC
import GHC.Core.Type (coreView)
import GHC.Core.TyCo.FVs (tyCoVarsOfType)
import GHC.Types.Var.Set (isEmptyVarSet)
import GHC.CoreToIface (toIfaceType)
import GHC.Iface.Syntax (IfaceDecl(..), IfaceIdDetails(..), freeNamesIfDecl)
import GHC.Iface.Binary (putWithUserData, getWithUserData, TraceBinIFace(..), CompressionIFace(..))
import GHC.IfaceToCore (tcIfaceDecl)
import GHC.Utils.Binary (openBinMem, withBinBuffer, unsafeUnpackBinBuffer)
import GHC.Core.TyCo.Rep (Type(..), TyLit(..))
import GHC.Data.FastString (unpackFS)
import GHC.Types.Var (VarBndr(..), ForAllTyFlag(..), Specificity(..), FunTyFlag(..), isTyVar, varType)
import Tidepool.TypePolicy (stabilizeEffectRows)
import GHC.Core.TyCon (tyConName)
import GHC.Driver.Env (lookupType, hsc_home_unit, hsc_NC)
import GHC.Iface.Env (lookupOrig)
import GHC.Iface.Load (importDecl)
import GHC.Tc.Utils.Monad (initIfaceLoad, initIfaceLcl)
import qualified GHC.Data.Maybe as MErr
import GHC.Types.Name (nameModule_maybe, nameOccName, wiredInNameTyThing_maybe)
import GHC.Types.Name.Occurrence
  ( isDataOcc, isTcOcc, mkVarOcc, occNameString )
import GHC.Types.Name.Reader (rdrNameOcc)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Unit.Types (unitString)
import GHC.Unit.Home (isHomeUnit, mkHomeModule)
import GHC.Utils.Outputable hiding ((<>), text)
import qualified GHC.Utils.Outputable as Outputable

-- The human-readable type is presentation only. The compiler consumes its own
-- binary IfaceType and resolves its Names against the admitted environment.
-- The payload is producer-pinned by the enclosing checked receipt.
data CheckedSignature = CheckedSignature
  { signatureKey :: String
  , signaturePresentation :: String
  , signatureInterface :: BS.ByteString
  , signatureNames :: [CheckedSignatureName]
  } deriving (Eq, Ord, Show)

data CheckedSignatureName = CheckedSignatureName
  { signatureUnit :: String
  , signatureModule :: String
  , signatureNamespace :: String
  , signatureOccurrence :: String
  } deriving (Eq, Ord, Show)

interfaceNames :: IfaceDecl -> [CheckedSignatureName]
interfaceNames = sortOn id . map identity . nonDetEltsUniqSet . freeNamesIfDecl
  where
    identity name = case nameModule_maybe name of
      Just owner -> CheckedSignatureName
        (unitString (moduleUnit owner)) (moduleNameString (moduleName owner))
        (if isDataOcc occurrence then "data" else if isTcOcc occurrence then "type" else "var")
        (occNameString occurrence)
        where occurrence = nameOccName name
      Nothing -> error "checked interface type contains a non-external Name"

captureCheckedSignature :: HscEnv -> String -> Type -> IO CheckedSignature
captureCheckedSignature env key ty = do
  unless (isEmptyVarSet (tyCoVarsOfType ty)) (fail "checked signature contains free type/coercion variables")
  binder <- initIfaceLoad env (lookupOrig
    (mkHomeModule (hsc_home_unit env) (mkModuleName "Tidepool.CheckedAnnotation")) (mkVarOcc key))
  let interface = IfaceId binder (toIfaceType ty) IfVanillaId []
      inventory = interfaceNames interface
  unless (length inventory <= 65536) (fail "checked signature Name bound")
  buffer <- openBinMem 1024
  putWithUserData QuietBinIFace NormalCompression buffer interface
  bytes <- withBinBuffer buffer (pure . BS.copy)
  unless (BS.length bytes <= 4 * 1024 * 1024) (fail "checked signature interface bound")
  pure (CheckedSignature key (renderWithContext defaultSDocContext (ppr ty)) bytes inventory)

encodeCheckedSignature :: CheckedSignature -> Encoding
encodeCheckedSignature signature = encodeListLen 5
  <> text "TPCHECKEDSIGNATURE2" <> text (signatureKey signature)
  <> text (signaturePresentation signature) <> encodeBytes (signatureInterface signature)
  <> encodeListLen (fromIntegral (length (signatureNames signature)))
  <> foldMap (\entry -> encodeListLen 4
      <> text (signatureUnit entry) <> text (signatureModule entry)
      <> text (signatureNamespace entry) <> text (signatureOccurrence entry)) (signatureNames signature)
  where text = encodeString . T.pack

resolveCheckedSignature :: HscEnv -> CheckedSignature -> IO Type
resolveCheckedSignature env signature = do
  unless (not (BS.null bytes) && BS.length bytes <= 4 * 1024 * 1024)
    (fail "checked signature interface bound")
  buffer <- unsafeUnpackBinBuffer bytes
  interface <- getWithUserData (hsc_NC env) buffer
  case interface of
    IfaceId {ifName = binder, ifIdDetails = IfVanillaId, ifIdInfo = []}
      | occNameString (nameOccName binder) == signatureKey signature
      , nameModule_maybe binder == Just (mkHomeModule (hsc_home_unit env)
          (mkModuleName "Tidepool.CheckedAnnotation")) -> pure ()
    _ -> fail "checked signature contains a non-signature declaration"
  unless (interfaceNames interface == signatureNames signature)
    (fail "checked signature interface Name inventory mismatch")
  -- Resolve the complete interface dependency inventory before type hydration.
  -- A hidden home dependency must already have been admitted; package Names
  -- retain normal interface loading under the pinned package environment.
  forM_ (nonDetEltsUniqSet (freeNamesIfDecl interface)) $ \name -> do
    owner <- maybe (fail "checked signature contains a local Name") pure (nameModule_maybe name)
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
      ++ unitString (moduleUnit owner) ++ ":" ++ moduleNameString (moduleName owner)
      ++ ":" ++ occNameString (nameOccName name)))
  resolved <- initIfaceLoad env $ initIfaceLcl
    (mkHomeModule (hsc_home_unit env) (mkModuleName "Tidepool.CheckedAnnotation"))
    (Outputable.text "checked signature") NotBoot (tcIfaceDecl True interface)
  case resolved of
    AnId identifier -> do
      let ty = idType identifier
      unless (isEmptyVarSet (tyCoVarsOfType ty)) (fail "checked signature resolved free type/coercion variables")
      pure ty
    _ -> fail "checked signature did not resolve to an Id"
  where bytes = signatureInterface signature

-- Only compiler-generated signature binders acquire exact Types. Authored
-- source continues through ordinary GHC renaming and lexical lookup.
rewriteCheckedAnnotations
  :: HscEnv -> [(String, CheckedSignature)] -> ParsedModule -> IO ParsedModule
rewriteCheckedAnnotations env annotations parsed = do
  resolved <- forM annotations $ \(binder, signature) ->
    (,) binder <$> resolveCheckedSignature env signature
  counts <- newIORef []
  rewritten <- everywhereM (mkM (rewriteSignature counts resolved)) (pm_parsed_source parsed)
  seen <- readIORef counts
  unless (sortOn id seen == sortOn id (map fst annotations))
    (fail "generated checked annotation is missing or duplicated")
  pure parsed { pm_parsed_source = rewritten }
  where
    rewriteSignature :: IORef [String] -> [(String, Type)] -> Sig GhcPs -> IO (Sig GhcPs)
    rewriteSignature counts resolved signature@(TypeSig extension binders ty) =
      case [(binder, exact) | (binder, exact) <- resolved,
          map (occNameString . rdrNameOcc . unLoc) binders == [binder]] of
        [] -> pure signature
        [(binder, exact)] -> do
          modifyIORef' counts (binder :)
          pure (TypeSig extension binders (replaceBody exact ty))
        _ -> fail "duplicate checked annotation binder"
    rewriteSignature _ _ signature = pure signature
    replaceBody :: Type -> LHsSigWcType GhcPs -> LHsSigWcType GhcPs
    replaceBody exact (HsWC extension (L location (HsSig signatureExtension _ body))) =
      HsWC extension (L location (HsSig signatureExtension (HsOuterImplicit noExtField) (fmap (const (XHsType exact)) body)))

-- The rendering is presentation only; canonical equality is the complete
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

captureCheckedTypeWitness :: HscEnv -> Type -> IO (Maybe CheckedTypeWitness)
captureCheckedTypeWitness env original = case evalStateT (shape 0 [] stable) (0 :: Int) of
  Left _ -> pure Nothing
  Right (encoded, owners) -> do
    let bytes = toStrictByteString encoded
    if BS.length bytes > 4 * 1024 * 1024 then pure Nothing else do
      signature <- captureCheckedSignature env "activation-input" stable
      pure (Just (CheckedTypeWitness signature bytes
        (Map.elems (Map.fromList [(ownerIdentity owner, owner) | owner <- owners])) Nothing))
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
