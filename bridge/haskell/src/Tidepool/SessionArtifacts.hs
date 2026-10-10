module Tidepool.SessionArtifacts
  ( mkBoundBinders
  , PreparedSessionBindings
  , prepareSessionBindings
  , sessionBindingRepresentations
  , writeSessionBindings
  , PreparedTypedSegmentBindings
  , prepareTypedSegmentSessionBindings
  , typedSegmentSessionEnvironment, typedSegmentSessionGlobals
  , typedSegmentSessionInterfaces, typedSegmentSessionBinders
  , typedSegmentSessionRetainedGlobals, typedSegmentSessionInterfacesThrough
  , typedSegmentSessionBindingRepresentations
  , withTypedSegmentSessionPublication
  , emitHostBindingInterface
  , parseValModule
  ) where

import Control.Monad (forM_, unless, when)
import Control.Exception (bracket, mask, onException)
import Control.Monad (forM, foldM)
import Data.IORef (newIORef, modifyIORef', readIORef)
import Data.List (nub, isPrefixOf)
import Data.Data (Data, Typeable, cast, gmapQ)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import GHC
  ( GhcRn, HsBind, HsBindLR(..), LHsExpr, HsExpr(..), MatchGroup(..)
  , Match(..), GRHSs(..), GRHS(..), StmtLR(..), HsTupArg(..), FixitySig(..)
  , GenLocated(..), unLoc, Name )
import GHC.Tc.Types (TcGblEnv, tcg_rn_decls, tcg_mod)
import GHC.Types.Fixity (Fixity)
import qualified Data.ByteString as BS
import qualified Data.Text as T
import Codec.CBOR.Encoding (encodeListLen, encodeString)
import Codec.CBOR.Write (toStrictByteString)
import GHC.Core.Type (Type, tyConsOfType)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Types.Id (Id, idName, idType)
import GHC.Builtin.Names (gHC_PRIM)
import GHC.Core.TyCon (tyConName)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Driver.Env (HscEnv, hsc_home_unit)
import GHC.Types.Name.Occurrence (OccName, mkVarOcc, occNameString)
import Data.Word (Word64)
import System.IO (hPutStrLn, stderr, openTempFile, openBinaryTempFile, hClose, hIsClosed)
import System.Directory (doesPathExist, createDirectory, removeDirectoryRecursive, removeFile, createDirectoryIfMissing)
import System.FilePath (takeDirectory)
import System.Posix.Files (createLink)

import Tidepool.Binders (BoundBinder(..), ValueTier(..))
import Tidepool.GhcPipeline
  ( PipelineResult, prHscEnv, prCanonicalInterfaceAdmissions, prResultType, prTargetTcGblEnv, isClosureType
  , renderType, stripMonadHead, splitTupleType )
import Tidepool.Identity (stableVarId)
import Tidepool.HostBindingAuthority
  ( HostBindingRepresentation, hostBindingRepresentationForType
  , hostBindingRepresentationAuthority, resolveHostBindingAuthorities )
import Tidepool.Session
  ( Generation(..), SessionModule(..), SessionModuleKind(..)
  , mkThinSessionIfaceWithFixities, parseSessionModule, sessionBinderName
  , scaffoldTargetName, scaffoldOutputBase
  , sessionModuleString, writeSessionIface, injectSessionIfaceWithBindings
  , CapturedSessionInterface, capturedSessionInterface, capturedSessionInterfaceEvidence )
import Tidepool.Session (sessionHiPath)
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), ExactInterfaceOperations, runExactInterfaceOperation, hydrateExactScope
  , newOriginalInterfaceArtifactsWithReader )
import Tidepool.ExactScope
  ( CanonicalInterfaceAdmission, ExactScope , scopeProducerSha256, scopeInterfaces, readExactScope, revalidateExactScope, scopeCanonicalInterfaces, readScopedInterfaces, scopeInterfaceToken, validateExactScopeEnvironment )
import Tidepool.CheckedCell (CheckedSignature, resolveCheckedSignature
  , captureCheckedTypeWitness, sealCheckedTypeWitness, encodeCheckedTypeWitness
  , validateOriginalInputTypeWitness, validateCheckedTypeWitnessBytes)
import Tidepool.DeclarationJoin (BindingInterfacePurpose(..))
import Tidepool.PackageWitness (PackageImportEvidence(..), CompilerProvidedImport(..), encodePackageImports, packageImportRoot)
import qualified Crypto.Hash.SHA256 as SHA256
import Numeric (showHex)
import Tidepool.TypePolicy (rootNominalHeadOfType, stabilizeEffectRows)
import Tidepool.TypedSegment.Types
  ( PendingTypedSegment, pendingSegmentItems, typedItemPlan, typedItemCaptures
  , TypedItemPlan(..), typedCaptureIdentifier, typedCaptureType
  , typedCaptureFixity )

-- The captured GHC types and their authority are resolved once, before native
-- projection, and retained unchanged for the later session interface write.
data PreparedSessionBindings = PreparedSessionBindings PipelineResult
  [(String, Type, Type, Maybe HostBindingRepresentation)]

prepareSessionBindings :: [String] -> PipelineResult -> IO PreparedSessionBindings
prepareSessionBindings [] result = pure (PreparedSessionBindings result [])
prepareSessionBindings bindNames result = do
  resultType <- case prResultType result of
    Just ty -> pure ty
    Nothing -> error "session bind has no captured result type"
  let valueType = stripMonadHead resultType
  componentTypes <- case bindNames of
    [_] -> pure [valueType]
    _ -> case splitTupleType valueType of
      Nothing -> error $ "multi-bind result is not a tuple: " ++ renderType valueType
      Just types
        | length types == length bindNames -> pure types
        | otherwise -> error $ "multi-bind has " ++ show (length bindNames)
            ++ " names but its result has " ++ show (length types) ++ " fields"
  let persistedTypes = map stabilizeEffectRows componentTypes
  authorities <- resolveHostBindingAuthorities persistedTypes (prHscEnv result)
    (prCanonicalInterfaceAdmissions result)
  pure (PreparedSessionBindings result
    (zipWith3 (\name ty persisted -> (name, ty, persisted,
      hostBindingRepresentationForType authorities persisted))
      bindNames componentTypes persistedTypes))

sessionBindingRepresentations :: PreparedSessionBindings -> [HostBindingRepresentation]
sessionBindingRepresentations (PreparedSessionBindings _ bindings) =
  [representation | (_, _, _, Just representation) <- bindings]

-- | Describe and publish the values materialized by one session bind.
mkBoundBinders :: [String] -> Word64 -> FilePath -> PipelineResult -> IO [BoundBinder]
mkBoundBinders names generation root result = do
  prepared <- prepareSessionBindings names result
  writeSessionBindings generation root prepared

writeSessionBindings :: Word64 -> FilePath -> PreparedSessionBindings -> IO [BoundBinder]
writeSessionBindings generation root (PreparedSessionBindings result bindings) = do
  fixities <- boundBinderFixities [name | (name, _, _, _) <- bindings] (prTargetTcGblEnv result)
  writeNativeSessionBindings (prHscEnv result) generation root bindings fixities

-- The compiler owns the captures; this product owns their thin transport and
-- actual hydrated global identifiers. It grants no authority to a live value.
data PreparedTypedSegmentBindings = PreparedTypedSegmentBindings HscEnv
  [PreparedTypedItemBindings] [HostBindingRepresentation]

data PreparedTypedItemBindings
  = UncapturedTypedItem Int
  | CapturedTypedItem Int SessionModule [BoundBinder] [(Id, Id)] CapturedSessionInterface

typedSegmentSessionEnvironment :: PreparedTypedSegmentBindings -> HscEnv
typedSegmentSessionEnvironment (PreparedTypedSegmentBindings env _ _) = env

typedSegmentSessionGlobals :: PreparedTypedSegmentBindings -> [(Id, Id)]
typedSegmentSessionGlobals (PreparedTypedSegmentBindings _ items _) =
  concat [globals | CapturedTypedItem _ _ _ globals _ <- items]

-- Native projection records these compiler-hydrated capture references as
-- runtime generations. The batch grants no completed value or source body.
typedSegmentSessionRetainedGlobals :: PreparedTypedSegmentBindings -> [(Id, Word64)]
typedSegmentSessionRetainedGlobals (PreparedTypedSegmentBindings _ items _) =
  [(global,generation)
  | CapturedTypedItem _ (SessionModule _ (Generation generation)) _ globals _ <- items
  , (_,global) <- globals]

-- A packet can publish only current/prior outputs from its exact ordered batch;
-- all binders of the same item/generation stay together.
typedSegmentSessionInterfacesThrough :: Int -> PreparedTypedSegmentBindings -> [CapturedSessionInterface]
typedSegmentSessionInterfacesThrough ordinal (PreparedTypedSegmentBindings _ items _) =
  [snapshot | CapturedTypedItem index _ _ _ snapshot <- items, index <= ordinal]

typedSegmentSessionInterfaces :: PreparedTypedSegmentBindings -> [CapturedSessionInterface]
typedSegmentSessionInterfaces (PreparedTypedSegmentBindings _ items _) =
  [captured | CapturedTypedItem _ _ _ _ captured <- items]

typedSegmentSessionBinders :: PreparedTypedSegmentBindings -> [(Int, [BoundBinder])]
typedSegmentSessionBinders (PreparedTypedSegmentBindings _ items _) =
  map itemBinders items
  where
    itemBinders (UncapturedTypedItem ordinal) = (ordinal, [])
    itemBinders (CapturedTypedItem ordinal _ binders _ _) = (ordinal, binders)

typedSegmentSessionBindingRepresentations :: PreparedTypedSegmentBindings -> [HostBindingRepresentation]
typedSegmentSessionBindingRepresentations (PreparedTypedSegmentBindings _ _ representations) = representations

-- Complete the batch transport before returning any hydrated global. The
-- staging root belongs to this request; it must not be the public session root.
prepareTypedSegmentSessionBindings
  :: HscEnv -> Map.Map (String, String) CanonicalInterfaceAdmission
  -> PendingTypedSegment -> FilePath -> IO PreparedTypedSegmentBindings
prepareTypedSegmentSessionBindings initial admitted segment stagingRoot = do
  let items = pendingSegmentItems segment
      captures = concatMap typedItemCaptures items
      persisted capture = stabilizeEffectRows (typedCaptureType capture)
      itemModule item = SessionModule ValMod (Generation (plannedItemGeneration (typedItemPlan item)))
      modules = [itemModule item | item <- items, not (null (typedItemCaptures item))]
      outputs = concatMap (sessionBindingPaths stagingRoot) modules
  unless (length modules == length (nub modules))
    (fail "typed segment repeats a session generation")
  forM_ outputs refuseExisting
  authorities <- resolveHostBindingAuthorities (map persisted captures) initial admitted
  let representation capture = hostBindingRepresentationForType authorities (persisted capture)
  (do
    forM_ items $ \item -> unless (null (typedItemCaptures item)) $ do
      let selected = typedItemCaptures item
          owner = itemModule item
          occurrence = nameOccName . idName . typedCaptureIdentifier
          fixities = [(occurrence capture, fixity) | capture <- selected
            , Just fixity <- [typedCaptureFixity capture]]
      iface <- mkThinSessionIfaceWithFixities initial owner
        [(occurrence capture, persisted capture) | capture <- selected] fixities
      writeSessionIface initial stagingRoot owner iface
      writeSessionBindingEvidence initial stagingRoot owner (map persisted selected)
    (env, reversed) <- foldM (hydrateItem persisted representation itemModule) (initial, []) items
    pure (PreparedTypedSegmentBindings env (reverse reversed)
      [value | capture <- captures, Just value <- [representation capture]]))
    `onException` removeExisting outputs
  where
    hydrateItem persisted representation itemModule (env, completed) item = do
      let selected = typedItemCaptures item
          ordinal = plannedItemOrdinal (typedItemPlan item)
          owner = itemModule item
      if null selected then pure (env, UncapturedTypedItem ordinal : completed) else do
        (hydrated, globals, captured) <- injectSessionIfaceWithBindings stagingRoot owner env
        let expectedOwner = fst (capturedSessionInterface captured)
            occurrences = map (nameOccName . idName) globals
        unless (length globals == length selected && length occurrences == length (nub occurrences)
          && all ((== Just expectedOwner) . nameModule_maybe . idName) globals)
          (fail "typed session interface has a different global binder inventory")
        pairs <- forM selected $ \capture -> do
          let original = typedCaptureIdentifier capture
          global <- case filter ((== nameOccName (idName original)) . nameOccName . idName) globals of
            [found] -> pure found
            _ -> fail "typed capture has no unique hydrated session global"
          unless (eqType (idType global) (persisted capture)
            && eqType (idType global) (idType original))
            (fail "typed capture differs from its hydrated session type")
          pure (original, global)
        let binders = zipWith (captureBinder owner representation) selected (map snd pairs)
        pure (hydrated, CapturedTypedItem ordinal owner binders pairs captured : completed)

    captureBinder owner representation capture global = BoundBinder
      (occNameString (nameOccName (idName global))) (stableVarId (idName global))
      (sessionModuleString owner)
      (if isClosureType (idType global) then RetainOpaque else ForceData)
      (renderType (typedCaptureType capture)) (rootNominalHeadOfType (idType global))
      (hostBindingRepresentationAuthority <$> representation capture)

-- Publish only the immutable bytes that supplied the substituted global Ids.
-- Exclusive links refuse an existing generation, including a racing publisher;
-- the masked link/record pair makes cancellation rollback own exactly its files.
-- Keep publication under the caller's final request completion. A receipt
-- refusal or cancellation after the last interface write still rolls back
-- this batch before the compiler operation can publish its successful state.
withTypedSegmentSessionPublication
  :: FilePath -> PreparedTypedSegmentBindings -> IO result -> IO result
withTypedSegmentSessionPublication root (PreparedTypedSegmentBindings _ items _) complete = do
  files <- fmap concat $ forM items $ \item -> case item of
    UncapturedTypedItem _ -> pure []
    CapturedTypedItem _ owner _ _ snapshot -> case capturedSessionInterfaceEvidence snapshot of
      Nothing -> fail "typed capture interface lacks complete binding evidence"
      Just (packages, requirements, _) -> do
        let path = sessionHiPath root owner
        pure [(path, snd (capturedSessionInterface snapshot)),
          (path ++ ".packages", packages), (path ++ ".requirements", requirements)]
  forM_ files (refuseExisting . fst)
  created <- newIORef []
  let publish (path, bytes) = mask $ \restore -> do
        createDirectoryIfMissing True (takeDirectory path)
        bracket (openBinaryTempFile (takeDirectory path) "typed-session.tmp")
          (\(temporary, handle) -> do
            closed <- hIsClosed handle
            unless closed (hClose handle)
            removeFile temporary)
          (\(temporary, handle) -> do
            restore (BS.hPut handle bytes >> hClose handle)
            createLink temporary path
            modifyIORef' created (path :))
  (forM_ files publish >> complete) `onException` (readIORef created >>= removeExisting)

sessionBindingPaths :: FilePath -> SessionModule -> [FilePath]
sessionBindingPaths root owner = let path = sessionHiPath root owner
  in [path, path ++ ".packages", path ++ ".requirements"]

refuseExisting :: FilePath -> IO ()
refuseExisting path = do
  exists <- doesPathExist path
  when exists (fail "session generation output already exists")

removeExisting :: [FilePath] -> IO ()
removeExisting paths = forM_ paths $ \path -> do
  exists <- doesPathExist path
  when exists (removeFile path)

-- Only retained compiler interfaces supply the signature's original Names.
-- This operation emits a fresh type-only value interface without compiling code.
emitHostBindingInterface
  :: ExactInterfaceOperations -> String -> Word64 -> String -> CheckedSignature -> FilePath -> FilePath
  -> BindingInterfacePurpose -> IO (BoundBinder, BindingInterfacePurpose)
emitHostBindingInterface operations producer generation name signature manifest root purpose = runExactInterfaceOperation operations $ \initial -> do
  scope <- readExactScope manifest >>= either fail pure
  unless (scopeProducerSha256 scope == producer) (fail "host interface producer differs from exact scope")
  validateExactScopeEnvironment initial scope >>= either fail pure
  let artifacts = [artifact | (artifact, _, _) <- scopeInterfaces scope]
      sessionModule = SessionModule ValMod (Generation generation)
      path = sessionHiPath root sessionModule
  unless (all ((/= sessionModuleString sessionModule) . exactModule) artifacts)
    (fail "host interface reservation already exists in exact scope")
  forM_ [path, path ++ ".packages", path ++ ".requirements"] $ \output -> do
    exists <- doesPathExist output
    when exists (fail "host interface reservation output already exists")
  loaded <- readScopedInterfaces initial scope artifacts >>= either fail pure
  hydrated <- hydrateExactScope initial loaded
  (ty, _) <- resolveCheckedSignature hydrated signature
  (representation, issuedPurpose) <- case purpose of
    HostBuilt -> do
      authorities <- resolveHostBindingAuthorities [ty] hydrated (scopeCanonicalInterfaces scope)
      representation <- maybe (fail "checked signature has no authenticated host representation") pure
        (hostBindingRepresentationForType authorities ty)
      pure (Just representation, HostBuilt)
    OriginalLiveInput offered -> withWitnessScratch $ \scratch -> do
      originalInterfaces <- newOriginalInterfaceArtifactsWithReader (scopeInterfaceToken scope) hydrated Map.empty artifacts [] [] scratch
      witness <- captureCheckedTypeWitness hydrated ty
        >>= maybe (fail "original input type has no canonical witness") pure
      sealed <- sealCheckedTypeWitness originalInterfaces witness
        >>= maybe (fail "original input type witness is unsealed") pure
      either fail pure (validateOriginalInputTypeWitness signature offered sealed)
      bytes <- maybe (fail "original input type witness is unsealed") (pure . toStrictByteString)
        (encodeCheckedTypeWitness sealed)
      either fail pure (validateCheckedTypeWitnessBytes bytes)
      pure (Nothing, OriginalLiveInput bytes)
  binders <- writeNativeSessionBindings hydrated generation root [(name, ty, ty, representation)] []
  revalidateExactScope hydrated scope >>= either fail pure
  case binders of
    [binder] -> pure (binder, issuedPurpose)
    _ -> fail "host interface did not issue exactly one binder"

 where
  withWitnessScratch = bracket acquire removeDirectoryRecursive
  acquire = do
    (path, handle) <- openTempFile (takeDirectory manifest) "original-input-witness"
    hClose handle
    -- Reserve the random name before creating the transaction-local directory.
    removeFile path
    createDirectory path
    pure path

writeNativeSessionBindings
  :: HscEnv -> Word64 -> FilePath
  -> [(String, Type, Type, Maybe HostBindingRepresentation)] -> [(OccName, Fixity)]
  -> IO [BoundBinder]
writeNativeSessionBindings hsc generation root bindings fixities = do
  let
      sessionModule = SessionModule ValMod (Generation generation)
      build (name, ty, persistedType, representation) =
        let
            occurrence = mkVarOcc name
            varId = stableVarId (sessionBinderName hsc sessionModule occurrence)
            moduleName = sessionModuleString sessionModule
            tier = if isClosureType persistedType then RetainOpaque else ForceData
            displayType = renderType ty
            rootHead = rootNominalHeadOfType persistedType
            hostAuthority = hostBindingRepresentationAuthority <$> representation
        in (BoundBinder name varId moduleName tier displayType rootHead hostAuthority, occurrence, persistedType)
      built = map build bindings
      binders = [binder | (binder, _, _) <- built]
  iface <- mkThinSessionIfaceWithFixities hsc sessionModule [(occ, ty) | (_, occ, ty) <- built] fixities
  writeSessionIface hsc root sessionModule iface
  writeSessionBindingEvidence hsc root sessionModule [persisted | (_, _, persisted, _) <- bindings]
  forM_ binders $ \(BoundBinder name varId moduleName tier displayType rootHead hostAuthority) ->
    hPutStrLn stderr $ "  Wrote session iface: " ++ moduleName ++ " (" ++ name
      ++ " :: " ++ displayType ++ ", " ++ show tier ++ ", root " ++ show rootHead
      ++ ", authority " ++ show hostAuthority ++ ", varId " ++ show varId ++ ")"
  pure binders

-- The same interface writer seals package and nominal type requirements for
-- both legacy single-item binding and the typed segment batch.
writeSessionBindingEvidence :: HscEnv -> FilePath -> SessionModule -> [Type] -> IO ()
writeSessionBindingEvidence hsc root sessionModule persistedTypes = do
  let path = sessionHiPath root sessionModule
      owners = nub [owner | ty <- persistedTypes
        , constructor <- nonDetEltsUniqSet (tyConsOfType ty)
        , Just owner <- [nameModule_maybe (tyConName constructor)]]
      home = homeUnitAsUnit (hsc_home_unit hsc)
      requirements = [(unitString (moduleUnit owner), moduleNameString (moduleName owner))
        | owner <- owners, moduleUnit owner == home]
  roots <- forM [owner | owner <- owners, moduleUnit owner /= home, owner /= gHC_PRIM] $ \owner ->
    packageImportRoot hsc owner >>= either fail pure
  bytes <- BS.readFile path
  let digest = concatMap (\byte -> let rendered = showHex byte "" in
        replicate (2 - length rendered) '0' ++ rendered) (BS.unpack (SHA256.hash bytes))
      artifact = ExactIfaceArtifact (unitString home) (sessionModuleString sessionModule) path digest requirements
      text = encodeString . T.pack
  BS.writeFile (path ++ ".packages") (encodePackageImports artifact (PackageImportEvidence roots [CompilerPrimitive | gHC_PRIM `elem` owners]))
  BS.writeFile (path ++ ".requirements") (toStrictByteString
    (encodeListLen (fromIntegral (length requirements))
      <> foldMap (\(unit,owner) -> encodeListLen 2 <> text unit <> text owner) requirements))

-- Resolve the compiler wrapper's returned variables before looking up fixities.
-- Renamed Names distinguish nested and shadowed declarations with the same
-- spelling; only compiler-owned type-annotation aliases preserve that identity.
boundBinderFixities :: [String] -> TcGblEnv -> IO [(OccName, Fixity)]
boundBinderFixities exported environment = case tcg_rn_decls environment of
  Nothing -> fail "session bind has no renamed binder evidence"
  Just renamed -> case
      [body | binding@FunBind { fun_id = name } <- collect renamed
        , occNameString (nameOccName (unLoc name)) `elem` [scaffoldTargetName, scaffoldOutputBase]
        , nameModule_maybe (unLoc name) == Just (tcg_mod environment)
        , Just body <- [bindingBody binding]] of
    [body] -> case unLoc (stripExpression body) of
      HsDo _ _ locatedStatements -> do
        let statements = unLoc locatedStatements
            aliases = Map.fromList
              [(unLoc name, unLoc targetName)
              | statement <- statements
              , LetStmt _ local <- [unLoc statement]
              , binding@FunBind { fun_id = name } <- collect local
              , "__tidepool_checked_annotation_" `isPrefixOf` occNameString (nameOccName (unLoc name))
              , Just rhs <- [bindingBody binding]
              , HsVar _ targetName <- [unLoc (stripExpression rhs)]]
            fixities = Map.fromList
              [(unLoc name, fixity)
              | FixitySig _ names fixity <- (collect body :: [FixitySig GhcRn])
              , name <- names]
            resolve seen name
              | Set.member name seen = fail "session checked binder alias is cyclic"
              | Just target <- Map.lookup name aliases = resolve (Set.insert name seen) target
              | otherwise = pure name
        returned <- case reverse statements of
          lastStatement : _ -> case unLoc lastStatement of
            LastStmt _ value _ _ -> returnNames value
            _ -> fail "session bind has no final renamed result"
          [] -> fail "session bind has an empty renamed result"
        original <- mapM (resolve Set.empty) returned
        if map (occNameString . nameOccName) original /= exported
          then fail "session result differs from its exported binder identities"
          else pure [(mkVarOcc occurrence, fixity)
            | (occurrence,name) <- zip exported original
            , Just fixity <- [Map.lookup name fixities]]
      _ -> pure []
    [] -> pure []
    _ -> fail "session bind has ambiguous renamed result owners"
  where
    returnNames :: LHsExpr GhcRn -> IO [Name]
    returnNames expression = case unLoc (stripExpression expression) of
      HsApp _ _ argument -> case unLoc (stripExpression argument) of
        ExplicitTuple _ arguments _ -> mapM tupleName arguments
        _ -> (:[]) <$> variableName argument
      _ -> fail "session bind does not return its compiler binder tuple"
    tupleName (Present _ value) = variableName value
    tupleName _ = fail "session bind returns an incomplete binder tuple"
    variableName expression = case unLoc (stripExpression expression) of
      HsVar _ name -> pure (unLoc name)
      _ -> fail "session bind returns a value without binder identity"

bindingBody :: HsBind GhcRn -> Maybe (LHsExpr GhcRn)
bindingBody FunBind { fun_matches = MG { mg_alts = alternatives } } =
  case unLoc alternatives of
    [L _ Match { m_pats = L _ [], m_grhss = GRHSs { grhssGRHSs = [L _ (GRHS _ [] body)] } }] -> Just body
    _ -> Nothing
bindingBody _ = Nothing

stripExpression :: LHsExpr GhcRn -> LHsExpr GhcRn
stripExpression expression = case unLoc expression of
  HsPar _ inner -> stripExpression inner
  ExprWithTySig _ inner _ -> stripExpression inner
  _ -> expression

collect :: (Data value, Typeable selected) => value -> [selected]
collect value = case cast value of
  Just selected -> [selected]
  Nothing -> concat (gmapQ collect value)

parseValModule :: String -> Maybe SessionModule
parseValModule source = case parseSessionModule source of
  Just moduleName@(SessionModule ValMod _) -> Just moduleName
  _ -> Nothing
