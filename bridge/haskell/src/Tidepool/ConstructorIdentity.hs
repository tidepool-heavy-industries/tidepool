-- | GHC-owned immutable type evidence and constructor ID issuance.
module Tidepool.ConstructorIdentity
  ( IdentityResolver, captureIdentityResolver, unionIdentityResolvers
  , TypeIdentityFailure(..), resolveTypeIdentity
  , ResolvedTypeIdentity, resolveTypeIdentityEvidence, resolvedTypeIdentity
  , IssuedConstructor, issuedDataCon, issuedConstructorIdentity, issuedConstructorHostId
  , issuedIdentityResolver, issueConstructor, issueConstructorWithFamily, wiredInConstructor
  ) where

import Control.Exception (Exception)
import Control.Monad (when)
import Data.Bits ((.|.), shiftL)
import Data.ByteString qualified as BS
import Data.Map.Strict qualified as Map
import Data.Text (Text)
import Data.Word (Word64)
import Codec.CBOR.Write (toStrictByteString)
import Crypto.Hash.SHA256 qualified as SHA256
import GHC.Core.TyCon (TyCon, tyConName, isPromotedDataCon_maybe)
import GHC.Core.DataCon (DataCon, dataConName, dataConTyCon, dataConWorkId)
import GHC.Driver.Env (HscEnv, hsc_HPT, hscEPS)
import GHC.Types.Name (nameModule_maybe, nameOccName, isWiredInName)
import GHC.Types.Name.Occurrence (isDataOcc, mkTcOcc)
import GHC.Unit.Home.ModInfo (eltsHpt, hm_iface)
import GHC.Unit.External (ExternalPackageState(eps_PIT))
import GHC.Unit.Module.Env (moduleEnvToList)
import GHC.Unit.Module.ModIface (ModIface, mi_module, mi_final_exts, mi_hash_fn, mi_iface_hash)
import GHC.Unit.Module (moduleNameString)
import GHC.Unit.Types (Module, moduleName)
import GHC.Utils.Fingerprint (Fingerprint(..))
import System.Environment (lookupEnv)
import System.IO (hPutStrLn, stderr)
import Tidepool.Identity (nameSymbolIdentity, qualifiedName, varId)
import Tidepool.ExecutionSchema (SymbolIdentity(..), TypeIdentity(..), TypeVersion(..), ConstructorIdentity(..))
import Tidepool.ExecutionEncode (encodeConstructorIdentity)

-- Snapshots retain already selected immutable interfaces, never the mutable
-- compiler environment or a later request's interface selection.
newtype IdentityResolver = IdentityResolver (Map.Map Module [ModIface])

captureIdentityResolver :: HscEnv -> Maybe ModIface -> IO IdentityResolver
captureIdentityResolver env defining = do
  external <- hscEPS env
  let home = [(mi_module iface,iface) | info <- eltsHpt (hsc_HPT env), let iface = hm_iface info]
      explicit = [(mi_module iface,iface) | iface <- maybe [] pure defining]
  trace <- lookupEnv "TIDEPOOL_MEMO_TRACE"
  when (trace == Just "1") $ mapM_ (\(owner, iface) ->
    when (moduleNameString (moduleName owner) == "Tidepool.Effects.Core") $
      hPutStrLn stderr $ "nominal-interface-custody defining="
        ++ show (moduleNameString . moduleName . mi_module <$> defining)
        ++ " owner=" ++ show owner
        ++ " iface=" ++ show (mi_iface_hash (mi_final_exts iface))
        ++ " declarations=" ++ show
          [(name, mi_hash_fn (mi_final_exts iface) (mkTcOcc name))
          | name <- ["Worktree", "WorktreeSpec", "WorktreeError", "WorkspaceHandle",
              "WorktreeHandle", "WorktreeId", "WorktreeSummary", "SubmissionObservation",
              "BranchName", "GitOid", "MergeRequest", "MergeOutcome"]]) home
  pure (IdentityResolver (Map.map pure (Map.fromList (moduleEnvToList (eps_PIT external) ++ home ++ explicit))))

unionIdentityResolvers :: [IdentityResolver] -> IdentityResolver
unionIdentityResolvers resolvers = IdentityResolver (Map.unionsWith (++) [rows | IdentityResolver rows <- resolvers])

data TypeIdentityFailure
  = MissingTypeOwner Text
  | MissingTypeInterface SymbolIdentity
  | MissingTypeDeclarationHash SymbolIdentity
  | AmbiguousTypeDeclarationHash SymbolIdentity [TypeVersion]
  | ConstructorParentOwnerMismatch SymbolIdentity SymbolIdentity
  | ConstructorParentIdentityMismatch TypeIdentity TypeIdentity
  deriving (Eq, Show)
instance Exception TypeIdentityFailure

-- Only this module issues the evidence pairing a type identity with its
-- selected immutable interfaces and actual GHC parent declaration.
data ResolvedTypeIdentity = ResolvedTypeIdentity IdentityResolver TyCon TypeIdentity

resolvedTypeIdentity :: ResolvedTypeIdentity -> TypeIdentity
resolvedTypeIdentity (ResolvedTypeIdentity _ _ identity) = identity

resolveTypeIdentity :: IdentityResolver -> TyCon -> Either TypeIdentityFailure TypeIdentity
resolveTypeIdentity resolver tc = resolvedTypeIdentity <$> resolveTypeIdentityEvidence resolver tc

resolveTypeIdentityEvidence :: IdentityResolver -> TyCon -> Either TypeIdentityFailure ResolvedTypeIdentity
resolveTypeIdentityEvidence resolver@(IdentityResolver interfaces) tc = do
  let name = tyConName tc
      parent = maybe tc dataConTyCon (isPromotedDataCon_maybe tc)
      parentName = tyConName parent
  symbol <- maybe (Left (MissingTypeOwner (qualifiedName name))) Right (nameSymbolIdentity (if isDataOcc (nameOccName name) then "data" else "type") name)
  version <- if isWiredInName parentName then Right WiredIn else do
    owner <- maybe (Left (MissingTypeOwner (qualifiedName parentName))) Right (nameModule_maybe parentName)
    ifaces <- maybe (Left (MissingTypeInterface symbol)) Right (Map.lookup owner interfaces)
    versions <- traverse (\iface -> do
      (_,Fingerprint high low) <- maybe (Left (MissingTypeDeclarationHash symbol)) Right
        (mi_hash_fn (mi_final_exts iface) (nameOccName parentName))
      pure (GhcAbi high low)) ifaces
    case Map.keys (Map.fromList [(candidate,()) | candidate <- versions]) of
      [selected] -> pure selected
      distinct -> Left (AmbiguousTypeDeclarationHash symbol distinct)
  pure (ResolvedTypeIdentity resolver tc (TypeIdentity symbol version))

-- The compiler declaration and its issued identity remain one evidence value
-- through executable projection and metadata publication.
data IssuedConstructor = IssuedConstructor
  { issuedIdentityResolver :: IdentityResolver
  , issuedDataCon :: DataCon
  , issuedConstructorIdentity :: ConstructorIdentity
  , issuedConstructorHostId :: Word64
  }

issueConstructor :: IdentityResolver -> DataCon -> Either TypeIdentityFailure IssuedConstructor
issueConstructor resolver dc = do
  family <- resolveTypeIdentityEvidence resolver (dataConTyCon dc)
  issueConstructorWithFamily family dc

issueConstructorWithFamily :: ResolvedTypeIdentity -> DataCon -> Either TypeIdentityFailure IssuedConstructor
issueConstructorWithFamily (ResolvedTypeIdentity resolver parentTyCon family) dc = do
  symbol <- maybe (Left (MissingTypeOwner (qualifiedName (dataConName dc)))) Right
    (nameSymbolIdentity "constructor" (dataConName dc))
  let parent = typeSymbol family
  if (symbolUnit symbol,symbolModule symbol) /= (symbolUnit parent,symbolModule parent)
    then Left (ConstructorParentOwnerMismatch symbol parent) else pure ()
  actualParent <- maybe (Left (MissingTypeOwner (qualifiedName (tyConName (dataConTyCon dc))))) Right
    (nameSymbolIdentity "type" (tyConName (dataConTyCon dc)))
  if actualParent /= parent || dataConTyCon dc /= parentTyCon then Left (ConstructorParentIdentityMismatch family (TypeIdentity actualParent (typeVersion family)))
    else pure ()
  let identity = ConstructorIdentity symbol family
      bytes = BS.pack (map (fromIntegral . fromEnum) "tidepool-constructor-id/v1\0")
        <> toStrictByteString (encodeConstructorIdentity identity)
      digest = SHA256.hash bytes
      hashed = BS.foldl' (\acc byte -> (acc `shiftL` 8) .|. fromIntegral byte) 0 (BS.take 7 digest)
      host = if isWiredInName (dataConName dc) then varId (dataConWorkId dc)
        else (0xFE `shiftL` 56) .|. hashed
  pure (IssuedConstructor resolver dc identity host)

wiredInConstructor :: DataCon -> Either TypeIdentityFailure IssuedConstructor
wiredInConstructor = issueConstructor (IdentityResolver Map.empty)
