module Tidepool.SessionArtifacts
  ( mkBoundBinders
  , parseValModule
  ) where

import Control.Monad (forM_)
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
import GHC.Core.Type (tyConsOfType)
import GHC.Builtin.Names (gHC_PRIM)
import GHC.Core.TyCon (tyConName)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Driver.Env (hsc_home_unit)
import GHC.Types.Name.Occurrence (OccName, mkVarOcc, occNameString)
import Data.Word (Word64)
import System.IO (hPutStrLn, stderr)

import Tidepool.Binders (BoundBinder(..), ValueTier(..))
import Tidepool.GhcPipeline
  ( PipelineResult(..), isClosureType, renderType, stripMonadHead
  , splitTupleType )
import Tidepool.Identity (stableVarId)
import Tidepool.HostBindingAuthority
  ( classifyHostBindingAuthority, resolveHostBindingAuthorities )
import Tidepool.Session
  ( Generation(..), SessionModule(..), SessionModuleKind(..)
  , mkThinSessionIfaceWithFixities, parseSessionModule, sessionBinderName
  , scaffoldTargetName, scaffoldOutputBase
  , sessionModuleString, writeSessionIface )
import Tidepool.Session (sessionHiPath)
import Tidepool.ExactHydration (ExactIfaceArtifact(..))
import Tidepool.PackageWitness (PackageImportEvidence(..), CompilerProvidedImport(..), encodePackageImports, packageImportRoot)
import qualified Crypto.Hash.SHA256 as SHA256
import Numeric (showHex)
import Tidepool.TypePolicy (rootNominalHeadOfType, stabilizeEffectRows)

-- | Describe and publish the values materialized by one session bind. The
-- captured result type is split for multi-binds, checked for cross-compilation
-- safety, and written as the thin interface later turns import.
mkBoundBinders :: [String] -> Word64 -> FilePath -> PipelineResult -> IO [BoundBinder]
mkBoundBinders bindNames generation root result = do
  resultType <- case prResultType result of
    Just ty -> pure ty
    Nothing -> error "session bind has no captured result type"
  let hsc = prHscEnv result
      sessionModule = SessionModule ValMod (Generation generation)
      valueType = stripMonadHead resultType
  componentTypes <- case bindNames of
    [_] -> pure [valueType]
    _ -> case splitTupleType valueType of
      Nothing -> error $ "multi-bind result is not a tuple: " ++ renderType valueType
      Just types
        | length types == length bindNames -> pure types
        | otherwise -> error $ "multi-bind has " ++ show (length bindNames)
            ++ " names but its result has " ++ show (length types) ++ " fields"
  let persistedTypes = map stabilizeEffectRows componentTypes
  authorities <- resolveHostBindingAuthorities persistedTypes hsc
  let build name ty persistedType =
        let
            occurrence = mkVarOcc name
            varId = stableVarId (sessionBinderName hsc sessionModule occurrence)
            moduleName = sessionModuleString sessionModule
            tier = if isClosureType persistedType then RetainOpaque else ForceData
            displayType = renderType ty
            rootHead = rootNominalHeadOfType persistedType
            hostAuthority = classifyHostBindingAuthority authorities persistedType
        in (BoundBinder name varId moduleName tier displayType rootHead hostAuthority, occurrence, persistedType)
      built = zipWith3 build bindNames componentTypes persistedTypes
      binders = [binder | (binder, _, _) <- built]
  fixities <- boundBinderFixities bindNames (prTargetTcGblEnv result)
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
