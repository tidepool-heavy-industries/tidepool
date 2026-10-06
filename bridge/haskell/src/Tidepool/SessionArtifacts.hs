module Tidepool.SessionArtifacts
  ( mkBoundBinders
  , PreparedSessionBindings
  , prepareSessionBindings
  , sessionBindingRepresentations
  , writeSessionBindings
  , emitHostBindingInterface
  , parseValModule
  ) where

import Control.Monad (forM_, unless, when)
import Control.Exception (bracket)
import Control.Monad (forM)
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
import System.IO (hPutStrLn, stderr, openTempFile, hClose)
import System.Directory (doesPathExist, createDirectory, removeDirectoryRecursive, removeFile)
import System.FilePath (takeDirectory)

import Tidepool.Binders (BoundBinder(..), ValueTier(..))
import Tidepool.GhcPipeline
  ( PipelineResult(..), isClosureType, renderType, stripMonadHead
  , splitTupleType )
import Tidepool.Identity (stableVarId)
import Tidepool.HostBindingAuthority
  ( HostBindingRepresentation, hostBindingRepresentationForType
  , hostBindingRepresentationAuthority, resolveHostBindingAuthorities )
import Tidepool.Session
  ( Generation(..), SessionModule(..), SessionModuleKind(..)
  , mkThinSessionIfaceWithFixities, parseSessionModule, sessionBinderName
  , scaffoldTargetName, scaffoldOutputBase
  , sessionModuleString, writeSessionIface )
import Tidepool.Session (sessionHiPath)
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), freshExactState, readExactIfaceArtifacts, hydrateExactScope
  , newOriginalInterfaceArtifacts )
import Tidepool.ExactScope
  ( ExactScope(..), scopeInterfaces, readExactScope, revalidateExactScope, scopeCanonicalInterfaces )
import Tidepool.CheckedCell (CheckedSignature, resolveCheckedSignature
  , captureCheckedTypeWitness, sealCheckedTypeWitness, encodeCheckedTypeWitness
  , validateOriginalInputTypeWitness, validateCheckedTypeWitnessBytes)
import Tidepool.DeclarationJoin (BindingInterfacePurpose(..))
import Tidepool.PackageWitness (PackageImportEvidence(..), CompilerProvidedImport(..), encodePackageImports, packageImportRoot)
import qualified Crypto.Hash.SHA256 as SHA256
import Numeric (showHex)
import Tidepool.TypePolicy (rootNominalHeadOfType, stabilizeEffectRows)

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

-- Only retained compiler interfaces supply the signature's original Names.
-- This operation emits a fresh type-only value interface without compiling code.
emitHostBindingInterface
  :: HscEnv -> String -> Word64 -> String -> CheckedSignature -> FilePath -> FilePath
  -> BindingInterfacePurpose -> IO (BoundBinder, BindingInterfacePurpose)
emitHostBindingInterface initial producer generation name signature manifest root purpose = do
  scope <- readExactScope manifest >>= either fail pure
  unless (scopeProducerSha256 scope == producer) (fail "host interface producer differs from exact scope")
  fresh <- freshExactState initial
  revalidateExactScope fresh scope >>= either fail pure
  let artifacts = [artifact | (artifact, _, _) <- scopeInterfaces scope]
      sessionModule = SessionModule ValMod (Generation generation)
      path = sessionHiPath root sessionModule
  unless (all ((/= sessionModuleString sessionModule) . exactModule) artifacts)
    (fail "host interface reservation already exists in exact scope")
  forM_ [path, path ++ ".packages", path ++ ".requirements"] $ \output -> do
    exists <- doesPathExist output
    when exists (fail "host interface reservation output already exists")
  loaded <- readExactIfaceArtifacts fresh artifacts >>= either fail pure
  hydrated <- hydrateExactScope fresh loaded
  (ty, _) <- resolveCheckedSignature hydrated signature
  (representation, issuedPurpose) <- case purpose of
    HostBuilt -> do
      authorities <- resolveHostBindingAuthorities [ty] hydrated (scopeCanonicalInterfaces scope)
      representation <- maybe (fail "checked signature has no authenticated host representation") pure
        (hostBindingRepresentationForType authorities ty)
      pure (Just representation, HostBuilt)
    OriginalLiveInput offered -> withWitnessScratch $ \scratch -> do
      originalInterfaces <- newOriginalInterfaceArtifacts hydrated Map.empty artifacts scratch
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
      persistedTypes = [persisted | (_, _, persisted, _) <- bindings]
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
  forM_ binders $ \(BoundBinder name varId moduleName tier displayType rootHead hostAuthority) ->
    hPutStrLn stderr $ "  Wrote session iface: " ++ moduleName ++ " (" ++ name
      ++ " :: " ++ displayType ++ ", " ++ show tier ++ ", root " ++ show rootHead
      ++ ", authority " ++ show hostAuthority ++ ", varId " ++ show varId ++ ")"
  pure binders

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
