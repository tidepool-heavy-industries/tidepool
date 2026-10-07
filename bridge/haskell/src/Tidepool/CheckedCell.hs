{-# LANGUAGE ScopedTypeVariables, RankNTypes #-}

module Tidepool.CheckedCell
  ( CheckedSignature(..), CheckedSignatureName(..)
  , CellExpressionPlan(..), ExpressionLiftPlan(..)
  , encodeCellExpressionPlan, decodeCellExpressionPlan
  , captureCheckedSignature, encodeCheckedSignature, decodeCheckedSignature, resolveCheckedSignature
  , RequestTypeSignatures(..), RequestHelperRecipe(..), captureRequestTypeSignatures
  , encodeRequestTypeSignatures, decodeRequestTypeSignatures, renderRequestTypeSignatures
  , CheckedTypeWitness, captureCheckedTypeWitness, sealCheckedTypeWitness
  , encodeCheckedTypeWitness, renderCheckedTypeWitness
  , validateOriginalInputTypeWitness, validateCheckedTypeWitnessBytes
  , NativeParsedModule(..), unannotatedModule, mapNativeModule, thenNativeModule, typecheckNativeModule
  , typecheckNativeModuleWithDiagnostics
  , rewriteCheckedAnnotations, rewriteHostInputType, rewriteRequestTypes
  ) where

import Codec.CBOR.Encoding (Encoding, encodeListLen, encodeString, encodeBytes, encodeNull, encodeInt)
import qualified Codec.CBOR.Decoding as D
import Codec.CBOR.Decoding
  ( Decoder, TokenType(TypeNull), decodeListLen, decodeString, decodeBytes
  , decodeNull, peekTokenType, peekByteOffset, decodeInt )
import Codec.CBOR.Write (toStrictByteString)
import Codec.CBOR.Read (deserialiseFromBytes)
import qualified Data.ByteString.Lazy as BL
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import Tidepool.ExactHydration (OriginalInterfaceArtifacts, originalInterfaceSha256
  , exactHomeInstancesFor, withExactHomeInstances)
import Data.Maybe (catMaybes)
import Numeric (showHex)
import Control.Monad (forM, forM_, unless, replicateM)
import Data.IORef
import Data.List (sortOn)
import qualified Data.Text as T
import Data.Generics (Data, cast, gmapM, mkM)
import GHC
import GHC.Core.TyCo.FVs (tyCoVarsOfType)
import GHC.Core.TyCo.Tidy (tidyTopType)
import GHC.Types.Var.Set (isEmptyVarSet)
import GHC.CoreToIface (toIfaceType)
import GHC.Iface.Syntax (IfaceDecl(..), IfaceIdDetails(..), freeNamesIfDecl)
import GHC.Iface.Binary (putWithUserData, getWithUserData, TraceBinIFace(..), CompressionIFace(..))
import GHC.IfaceToCore (tcIfaceDecl)
import GHC.Utils.Binary (openBinMem, withBinBuffer, unsafeUnpackBinBuffer)
import Tidepool.TypePolicy (stabilizeEffectRows, NominalHead(..))
import Tidepool.CanonicalTypeShape (captureClosedTypeShape, canonicalShapeExpressionBytes, canonicalShapeOwners)
import GHC.Driver.Env (lookupType, hsc_home_unit, hsc_NC, hscSetFlags)
import GHC.Driver.Main (hscTypecheckRenameWithDiagnostics)
import GHC.Driver.Errors.Types (GhcMessage)
import GHC.Types.Error (Messages)
import GHC.Tc.Types (TcGblEnv)
import GHC.Tc.Module (RenamedStuff)
import GHC.Iface.Env (lookupOrig)
import GHC.Iface.Load (importDecl)
import GHC.Tc.Utils.Monad (initIfaceLoad, initIfaceLcl)
import qualified GHC.Data.Maybe as MErr
import GHC.Types.Name (nameModule_maybe, nameOccName, wiredInNameTyThing_maybe)
import GHC.Types.Name.Occurrence
  ( isDataOcc, isTcOcc, mkVarOcc, occNameString )
import GHC.Types.Name.Reader (rdrNameOcc)
import GHC.Types.Name.Set (NameSet, emptyNameSet, isEmptyNameSet, unionNameSets, usesOnly)
import GHC.Rename.Module (addTcgDUs)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Unit.Types (unitString)
import GHC.Unit.Home (isHomeUnit, mkHomeModule)
import GHC.Utils.Outputable hiding ((<>), text)
import qualified GHC.Utils.Outputable as Outputable

data ExpressionLiftPlan = ExpressionEffectful | ExpressionPure
  deriving (Eq, Show)

-- | Compiler-owned execution decision for one expression item. The key is
-- the reserved local binder whose zonked type supplied this evidence.
data CellExpressionPlan = CellExpressionPlan
  { expressionPlanKey :: String
  , expressionPlanLift :: ExpressionLiftPlan
  , expressionPlanType :: String
  , expressionPlanHeads :: [NominalHead]
  } deriving (Eq, Show)

encodeCellExpressionPlan :: CellExpressionPlan -> Encoding
encodeCellExpressionPlan CellExpressionPlan
  { expressionPlanKey = key
  , expressionPlanLift = liftPlan
  , expressionPlanType = ty
  , expressionPlanHeads = heads
  } =
  encodeListLen 4
  <> encodeString (T.pack key)
  <> encodeString (case liftPlan of
       ExpressionEffectful -> "effectful"
       ExpressionPure -> "pure")
  <> encodeString (T.pack ty)
  <> encodeListLen (fromIntegral (length heads))
  <> foldMap (\(NominalHead unit modul name) ->
      encodeListLen 3 <> encodeString unit <> encodeString modul <> encodeString name) heads

-- Decode the same compiler-owned plan carried unchanged through checked-item
-- admission. Rendered types and heads remain observations, not native evidence.
decodeCellExpressionPlan :: D.Decoder s CellExpressionPlan
decodeCellExpressionPlan = do
  fields <- D.decodeListLen
  unless (fields == 4) (fail "invalid cell expression plan row")
  key <- nonempty
  liftPlan <- D.decodeString >>= \value -> case value of
    "effectful" -> pure ExpressionEffectful
    "pure" -> pure ExpressionPure
    _ -> fail "invalid cell expression lift"
  ty <- T.unpack <$> D.decodeString
  count <- D.decodeListLen
  unless (count <= 65536) (fail "cell expression heads exceed bound")
  heads <- replicateM count $ do
    headFields <- D.decodeListLen
    unless (headFields == 3) (fail "invalid cell expression head")
    NominalHead <$> nonemptyText <*> nonemptyText <*> nonemptyText
  pure (CellExpressionPlan key liftPlan ty heads)
  where
    nonemptyText = do
      value <- D.decodeString
      unless (not (T.null value)) (fail "empty cell expression identity")
      pure value
    nonempty = T.unpack <$> nonemptyText

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

data RequestTypeSignatures = RequestTypeSignatures
  { requestReplySignature :: CheckedSignature
  , requestProgressSignature :: Maybe CheckedSignature
  } deriving (Eq, Show)

-- The protected source recipe selects helper presence independently of the
-- original request bundle's custody. Progress presence remains bundle-owned.
data RequestHelperRecipe = NoRequestHelpers | ActorReplyHelpers
  deriving (Eq, Show)

captureRequestTypeSignatures :: HscEnv -> Type -> Maybe Type -> IO RequestTypeSignatures
captureRequestTypeSignatures env reply progress = do
  replySignature <- captureCheckedSignature env "request-reply" reply
  progressSignature <- traverse (captureCheckedSignature env "request-progress") progress
  let signatures = RequestTypeSignatures replySignature progressSignature
  unless (BS.length (toStrictByteString (encodeRequestTypeSignatures signatures)) <= 4 * 1024 * 1024)
    (fail "request type signatures exceed four MiB")
  pure signatures

encodeRequestTypeSignatures :: RequestTypeSignatures -> Encoding
encodeRequestTypeSignatures signatures = encodeListLen 4
  <> encodeString (T.pack "TPREQUESTTYPESIGNATURES1") <> encodeString (T.pack "1")
  <> encodeCheckedSignature (requestReplySignature signatures)
  <> maybe encodeNull encodeCheckedSignature (requestProgressSignature signatures)

decodeRequestTypeSignatures :: Decoder s RequestTypeSignatures
decodeRequestTypeSignatures = boundedTypeEvidence $ do
  typeEvidenceRow 4
  magic <- typeEvidenceText
  version <- typeEvidenceText
  unless (magic == "TPREQUESTTYPESIGNATURES1" && version == "1")
    (fail "request type signatures version")
  reply <- decodeCheckedSignature
  token <- peekTokenType
  progress <- if token == TypeNull
    then decodeNull >> pure Nothing
    else Just <$> decodeCheckedSignature
  unless (signatureKey reply == "request-reply"
      && maybe True ((== "request-progress") . signatureKey) progress)
    (fail "request type signature purpose")
  pure (RequestTypeSignatures reply progress)

renderRequestTypeSignatures :: RequestTypeSignatures -> String
renderRequestTypeSignatures = hexBytes . toStrictByteString . encodeRequestTypeSignatures

decodeCheckedSignature :: Decoder s CheckedSignature
decodeCheckedSignature = boundedTypeEvidence $ do
  typeEvidenceRow 5
  magic <- typeEvidenceText
  unless (magic == "TPCHECKEDSIGNATURE2") (fail "checked signature version")
  key <- typeEvidenceText
  presentation <- typeEvidenceText
  interface <- decodeBytes
  unless (not (BS.null interface) && BS.length interface <= 4 * 1024 * 1024)
    (fail "checked signature interface bound")
  count <- decodeListLen
  unless (count <= 65536) (fail "checked signature Name bound")
  names <- replicateM count $ do
    typeEvidenceRow 4
    name <- CheckedSignatureName <$> typeEvidenceText <*> typeEvidenceText
      <*> typeEvidenceText <*> typeEvidenceText
    unless (signatureNamespace name `elem` ["type", "data", "var"])
      (fail "checked signature Name namespace")
    pure name
  unless (and (zipWith (<) names (drop 1 names)))
    (fail "checked signature Names are unsorted or duplicated")
  pure (CheckedSignature key presentation interface names)

-- Count the original wire span, including all Names and presentation, rather
-- than trusting the native interface bound or a smaller re-encoding.
boundedTypeEvidence :: Decoder s a -> Decoder s a
boundedTypeEvidence decoder = do
  before <- peekByteOffset
  value <- decoder
  after <- peekByteOffset
  unless (after - before <= 4 * 1024 * 1024) (fail "native type evidence exceeds four MiB")
  pure value

typeEvidenceRow :: Int -> Decoder s ()
typeEvidenceRow count = do
  actual <- decodeListLen
  unless (actual == count) (fail "invalid native type evidence row")

typeEvidenceText :: Decoder s String
typeEvidenceText = do
  value <- T.unpack <$> decodeString
  unless (not (null value)) (fail "empty native type evidence field")
  pure value

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
  -- The interface codec resolves local type variables by OccName, so distinct
  -- captured forall binders need GHC's scoped names before serialization.
  let interface = IfaceId binder (toIfaceType (tidyTopType ty)) IfVanillaId []
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

-- The parsed syntax and the generated types' dependency uses travel together.
-- HsCoreTy bypasses ordinary name renaming, so its external Names must be
-- registered before GHC builds usage fingerprints or desugars the module.
data NativeParsedModule = NativeParsedModule
  { nativeParsedModule :: ParsedModule
  , nativeTypeUses :: NameSet
  }

unannotatedModule :: ParsedModule -> NativeParsedModule
unannotatedModule parsed = NativeParsedModule parsed emptyNameSet

mapNativeModule :: (ParsedModule -> IO ParsedModule) -> NativeParsedModule -> IO NativeParsedModule
mapNativeModule transform annotated = do
  parsed <- transform (nativeParsedModule annotated)
  pure annotated { nativeParsedModule = parsed }

thenNativeModule :: NativeParsedModule -> (ParsedModule -> IO NativeParsedModule) -> IO NativeParsedModule
thenNativeModule first transform = do
  next <- transform (nativeParsedModule first)
  pure next { nativeTypeUses = unionNameSets [nativeTypeUses first, nativeTypeUses next] }

typecheckNativeModule :: NativeParsedModule -> Ghc TypecheckedModule
typecheckNativeModule annotated = do
  typed <- withExactHomeInstances (pm_mod_summary (nativeParsedModule annotated))
    (typecheckModule (nativeParsedModule annotated))
  let (environment, details) = tm_internals_ typed
  pure typed { tm_internals_ = (addNativeTypeUses annotated environment, details) }

-- Compiler phase hooks retain GHC's diagnostics instead of using the facade's
-- printing typecheck operation. Both entry points register the same native uses.
typecheckNativeModuleWithDiagnostics
  :: HscEnv -> NativeParsedModule
  -> IO ((TcGblEnv, RenamedStuff), Messages GhcMessage)
typecheckNativeModuleWithDiagnostics env annotated = do
  let parsed = nativeParsedModule annotated
      summary = pm_mod_summary parsed
      local = exactHomeInstancesFor summary (hscSetFlags (ms_hspp_opts summary) env)
      source = HsParsedModule
        { hpm_module = pm_parsed_source parsed
        , hpm_src_files = pm_extra_src_files parsed
        }
  ((environment, renamed), diagnostics) <- hscTypecheckRenameWithDiagnostics local summary source
  pure ((addNativeTypeUses annotated environment, renamed), diagnostics)

addNativeTypeUses :: NativeParsedModule -> TcGblEnv -> TcGblEnv
addNativeTypeUses annotated environment
  | isEmptyNameSet (nativeTypeUses annotated) = environment
  | otherwise = addTcgDUs environment (usesOnly (nativeTypeUses annotated))

-- Generic source rewrites stop at native types. Their semantic graphs are
-- compiler-owned data, not syntax to traverse again on a subsequent rewrite.
rewriteSyntax :: (forall a. Data a => a -> IO a) -> (forall a. Data a => a -> IO a)
rewriteSyntax transform value = case cast value :: Maybe (HsType GhcPs) of
  Just XHsType{} -> transform value
  _ -> gmapM (rewriteSyntax transform) value >>= transform

resolveCheckedSignature :: HscEnv -> CheckedSignature -> IO (Type, NameSet)
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
      pure (ty, freeNamesIfDecl interface)
    _ -> fail "checked signature did not resolve to an Id"
  where bytes = signatureInterface signature

-- Reserved leaves belong to admitted generated templates. Resolve each native
-- signature once, then rewrite all its occurrences in a single syntax walk.
data NativeTypeSlot = ActivationInputSlot | RequestReplySlot | RequestProgressSlot
  deriving (Eq, Ord)

slotOccurrence :: NativeTypeSlot -> String
slotOccurrence ActivationInputSlot = "TidepoolActivationInput"
slotOccurrence RequestReplySlot = "TidepoolRequestReply"
slotOccurrence RequestProgressSlot = "TidepoolRequestProgress"

rewriteNativeSlots
  :: HscEnv -> [(NativeTypeSlot, Int, Maybe CheckedSignature)]
  -> ParsedModule -> IO NativeParsedModule
rewriteNativeSlots env slots parsed = do
  unless (Map.size (Map.fromList [(slot, ()) | (slot, _, _) <- slots]) == length slots)
    (fail "generated recipe repeats a native type slot")
  resolved <- forM slots $ \(slot, expected, signature) -> do
    unless (expected >= 0 && (expected == 0) == maybe True (const False) signature)
      (fail "generated recipe has invalid native type slot evidence")
    native <- traverse (resolveCheckedSignature env) signature
    pure (slotOccurrence slot, (expected, native))
  counts <- newIORef Map.empty
  let inventory = Map.fromList resolved
  rewritten <- rewriteSyntax (mkM (replaceSlot counts inventory)) (pm_parsed_source parsed)
  actual <- readIORef counts
  forM_ resolved $ \(slot, (expected, _)) ->
    unless (Map.findWithDefault 0 slot actual == expected)
      (fail ("generated recipe has missing or duplicated native type slot: " ++ slot))
  pure (NativeParsedModule (parsed { pm_parsed_source = rewritten })
    (unionNameSets [names | (_, (_, Just (_, names))) <- resolved]))
  where
    replaceSlot :: IORef (Map.Map String Int) -> Map.Map String (Int, Maybe (Type, NameSet))
      -> HsType GhcPs -> IO (HsType GhcPs)
    replaceSlot counts inventory node@(HsTyVar _ promotion located) =
      let spelling = occNameString (rdrNameOcc (unLoc located))
      in case Map.lookup spelling inventory of
        Nothing -> pure node
        Just (_, Nothing) -> fail ("generated recipe has an unauthorized native type slot: " ++ spelling)
        Just (_, Just (exact, _)) -> case (promotion, unLoc located) of
          (NotPromoted, Unqual occurrence) | isTcOcc occurrence -> do
            modifyIORef' counts (Map.insertWith (+) spelling 1)
            pure (XHsType exact)
          _ -> fail ("generated native type slot is qualified or promoted: " ++ spelling)
    replaceSlot _ _ node = pure node

rewriteHostInputType :: HscEnv -> Int -> CheckedSignature -> ParsedModule -> IO NativeParsedModule
rewriteHostInputType env expected signature =
  rewriteNativeSlots env [(ActivationInputSlot, expected, Just signature)]

-- Reply occurs in both sessionReply and respond; progress occurs only in
-- reportProgress. No presentation string participates in these annotations.
rewriteRequestTypes
  :: HscEnv -> RequestHelperRecipe -> RequestTypeSignatures -> ParsedModule -> IO NativeParsedModule
rewriteRequestTypes env recipe signatures = rewriteNativeSlots env $ case recipe of
  NoRequestHelpers ->
    [ (RequestReplySlot, 0, Nothing)
    , (RequestProgressSlot, 0, Nothing)
    ]
  ActorReplyHelpers ->
    [ (RequestReplySlot, 2, Just (requestReplySignature signatures))
    , (RequestProgressSlot, maybe 0 (const 1) (requestProgressSignature signatures),
        requestProgressSignature signatures)
    ]

-- Only compiler-generated signature binders acquire exact Types. Authored
-- source continues through ordinary GHC renaming and lexical lookup.
rewriteCheckedAnnotations
  :: HscEnv -> [(String, CheckedSignature)] -> ParsedModule -> IO NativeParsedModule
rewriteCheckedAnnotations env annotations parsed = do
  resolved <- forM annotations $ \(binder, signature) ->
    (,) binder <$> resolveCheckedSignature env signature
  counts <- newIORef []
  let types = [(binder, ty) | (binder, (ty, _)) <- resolved]
      names = unionNameSets [used | (_, (_, used)) <- resolved]
  rewritten <- rewriteSyntax (mkM (rewriteSignature counts types)) (pm_parsed_source parsed)
  seen <- readIORef counts
  unless (sortOn id seen == sortOn id (map fst annotations))
    (fail "generated checked annotation is missing or duplicated")
  pure (NativeParsedModule (parsed { pm_parsed_source = rewritten }) names)
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
captureCheckedTypeWitness env original = case captureClosedTypeShape stable of
  Left _ -> pure Nothing
  Right structure -> do
    signature <- captureCheckedSignature env "activation-input" stable
    pure (Just (CheckedTypeWitness signature (canonicalShapeExpressionBytes structure)
      (canonicalShapeOwners structure) Nothing))
 where
  stable = stabilizeEffectRows original

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

-- Retain the offered native payload verbatim; a recaptured GHC binary signature
-- is not a canonical type fingerprint. Only semantic structure and exact seals
-- are compared with the independent capture from the admitted environment.
data OfferedTypeWitness = OfferedTypeWitness CheckedSignature BS.ByteString [(String, String, String)]

decodeOfferedTypeWitness :: BS.ByteString -> Either String OfferedTypeWitness
decodeOfferedTypeWitness bytes = do
  unless (not (BS.null bytes) && BS.length bytes <= 4 * 1024 * 1024)
    (Left "canonical input witness exceeds four MiB or is empty")
  (remaining, witness@(OfferedTypeWitness signature structure seals)) <-
    either (Left . show) Right (deserialiseFromBytes decoder (BL.fromStrict bytes))
  let text = encodeString . T.pack
      encoded = toStrictByteString (encodeListLen 5 <> text "TPCANONICALINPUTTYPE1" <> text "1"
        <> encodeCheckedSignature signature <> encodeBytes structure
        <> encodeListLen (fromIntegral (length seals))
        <> foldMap (\(unit, owner, digest) -> encodeListLen 3 <> text unit <> text owner <> text digest) seals)
  unless (BL.null remaining && encoded == bytes)
    (Left "canonical input witness must use canonical CBOR with no trailing bytes")
  (shapeRemaining, (shape, owners, _)) <- either (Left . show) Right
    (deserialiseFromBytes (decodeInputStructure 0 0) (BL.fromStrict structure))
  unless (BL.null shapeRemaining && toStrictByteString shape == structure)
    (Left "canonical input structure must use canonical CBOR with no trailing bytes")
  unless (Map.keys (Map.fromList [(owner, ()) | owner <- owners]) == [(unit, owner) | (unit, owner, _) <- seals])
    (Left "canonical input interface inventory differs from structure")
  pure witness
 where
  decoder = boundedTypeEvidence $ do
    typeEvidenceRow 5
    magic <- typeEvidenceText
    version <- typeEvidenceText
    unless (magic == "TPCANONICALINPUTTYPE1" && version == "1")
      (fail "canonical input witness version")
    signature <- decodeCheckedSignature
    unless (signatureKey signature == "activation-input") (fail "canonical input signature purpose")
    structure <- decodeBytes
    unless (not (BS.null structure) && BS.length structure <= 4 * 1024 * 1024)
      (fail "canonical input structure bound")
    count <- decodeListLen
    unless (count <= 65536) (fail "canonical input interface bound")
    seals <- replicateM count $ do
      typeEvidenceRow 3
      unit <- typeEvidenceText
      owner <- typeEvidenceText
      digest <- typeEvidenceText
      unless (length digest == 64 && all (`elem` (['0'..'9'] ++ ['a'..'f'])) digest)
        (fail "canonical input interface digest")
      pure (unit, owner, digest)
    let owners = [(unit, owner) | (unit, owner, _) <- seals]
    unless (and (zipWith (<) owners (drop 1 owners)))
      (fail "canonical input interface seals are unsorted or duplicated")
    pure (OfferedTypeWitness signature structure seals)

-- Bound depth before descending, including malicious embedded expressions.
-- Re-encoding the validated grammar rejects indefinite/noncanonical forms.
decodeInputStructure :: Int -> Int -> Decoder s (Encoding, [(String, String)], Int)
decodeInputStructure depth bound = do
  unless (depth <= 128) (fail "canonical input structure depth bound")
  count <- decodeListLen
  tag <- typeEvidenceText
  let text = encodeString . T.pack
      prefix = encodeListLen (fromIntegral count) <> text tag
      child = decodeInputStructure (depth + 1) bound
      combine encoding children = do
        let nodes = 1 + sum [size | (_, _, size) <- children]
        unless (nodes <= 65536) (fail "canonical input structure node bound")
        pure (encoding <> foldMap (\(value, _, _) -> value) children,
          concat [owners | (_, owners, _) <- children], nodes)
      flagged limit = do
        flag <- decodeInt
        unless (flag >= 0 && flag <= limit) (fail "canonical input structure flag")
        pure flag
  case (count, tag) of
    (2, "bound") -> do
      index <- decodeInt
      unless (index >= 0 && index < bound) (fail "canonical input free variable")
      pure (prefix <> encodeInt index, [], 1)
    (3, "con") -> do
      typeEvidenceRow 4
      unit <- typeEvidenceText
      owner <- typeEvidenceText
      namespace <- typeEvidenceText
      occurrence <- typeEvidenceText
      unless (namespace `elem` ["type", "data"]) (fail "canonical input Name namespace")
      arguments <- decodeListLen
      unless (arguments <= 65536) (fail "canonical input argument bound")
      children <- replicateM arguments child
      (encoded, owners, nodes) <- combine (prefix <> encodeListLen 4 <> text unit <> text owner
        <> text namespace <> text occurrence <> encodeListLen (fromIntegral arguments)) children
      pure (encoded, (unit, owner) : owners, nodes)
    (3, "app") -> replicateM 2 child >>= combine prefix
    (5, "fun") -> do
      flag <- flagged 3
      replicateM 3 child >>= combine (prefix <> encodeInt flag)
    (4, "forall") -> do
      flag <- flagged 2
      kind <- child
      body <- decodeInputStructure (depth + 1) (bound + 1)
      combine (prefix <> encodeInt flag) [kind, body]
    (3, "literal") -> do
      literal <- typeEvidenceText
      value <- case literal of
        "nat" -> do
          number <- typeEvidenceText
          unless (all (`elem` ['0'..'9']) number && (number == "0" || head number /= '0'))
            (fail "canonical input natural literal")
          pure (text number)
        "symbol" -> encodeString <$> decodeString
        "char" -> do
          character <- decodeInt
          unless (character >= 0 && character <= 0x10ffff && not (character >= 0xd800 && character <= 0xdfff))
            (fail "canonical input character literal")
          pure (encodeInt character)
        _ -> fail "canonical input literal tag"
      pure (prefix <> text literal <> value, [], 1)
    _ -> fail "canonical input structure tag or arity"

validateCheckedTypeWitnessBytes :: BS.ByteString -> Either String ()
validateCheckedTypeWitnessBytes bytes = () <$ decodeOfferedTypeWitness bytes

validateOriginalInputTypeWitness :: CheckedSignature -> BS.ByteString -> CheckedTypeWitness -> Either String ()
validateOriginalInputTypeWitness signature offered captured = do
  OfferedTypeWitness original structure seals <- decodeOfferedTypeWitness offered
  unless (original == signature) (Left "original input witness native signature differs from offer")
  interfaces <- maybe (Left "original input type witness is unsealed") Right (witnessInterfaces captured)
  let actual = [(unitString (moduleUnit owner), moduleNameString (moduleName owner), digest)
        | (owner, digest) <- interfaces]
  unless (structure == witnessStructure captured && seals == actual)
    (Left "original input type structure or original interface seals differ from offer")

renderCheckedTypeWitness :: CheckedTypeWitness -> Maybe String
renderCheckedTypeWitness witness = hexBytes . toStrictByteString <$> encodeCheckedTypeWitness witness

hexBytes :: BS.ByteString -> String
hexBytes = concatMap (\byte -> let value = showHex byte "" in if length value == 1 then '0' : value else value) . BS.unpack
