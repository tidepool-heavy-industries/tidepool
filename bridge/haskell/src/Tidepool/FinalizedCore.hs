-- | A source-free, compiler-native companion to a skinny exact interface.
-- The interface supplies declaration and instance authority; this companion
-- supplies the Core from the same tidy result, in its original binding groups.
module Tidepool.FinalizedCore
  ( FinalizedCoreFailure(..)
  , UnsupportedCoreMetadata(..)
  , isUnsupportedFinalizedCore
  , captureFinalizedCore
  , decodeFinalizedCore
  , attachFinalizedCore
  , finalizedCoreSiteInstances
  ) where

import Control.Exception (Exception, displayException)
import Data.ByteString qualified as BS
import Data.IORef (newIORef)
import Data.Maybe (isJust)
import Data.Set qualified as Set
import Data.Word (Word8)
import GHC.Core (bindersOfBinds)
import GHC.Core.InstEnv (ClsInst, instEnvElts)
import GHC.Core.TyCon (TyCon, tyConName)
import GHC.CoreToIface (toIfaceTopBind)
import GHC.Driver.Env (HscEnv(..))
import GHC.Driver.Env.KnotVars (knotVarsFromModuleEnv)
import GHC.Fingerprint.Type (Fingerprint)
import GHC.Iface.Binary
  ( CompressionIFace(..), TraceBinIFace(..), getWithUserData, putWithUserData )
import GHC.Iface.Syntax (IfaceBindingX, IfaceMaybeRhs, IfaceTopBndrInfo)
import GHC.IfaceToCore (typecheckWholeCoreBindings)
import GHC.Settings.Config (cProjectVersion)
import GHC.Tc.Utils.Monad (initIfaceCheck)
import GHC.Types.ForeignStubs (ForeignStubs(..), CHeader(..), CStub(..))
import GHC.Driver.Ppr (showSDoc)
import GHC.Types.Name (Name, isExternalName, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.TyThing (TyThing(..))
import GHC.Types.TypeEnv (lookupTypeEnv)
import GHC.Types.Var (varName)
import GHC.Unit.Home.ModInfo (HomeModInfo(..))
import GHC.Unit.Module.Env (mkModuleEnv)
import GHC.Unit.Module.Location (ModLocation)
import GHC.Unit.Module.ModDetails (md_types, md_insts)
import GHC.Unit.Module.ModGuts (CgGuts(..))
import GHC.Unit.Module.ModIface (ModIface, mi_module, mi_iface_hash, mi_final_exts, set_mi_extra_decls)
import GHC.Unit.Module.WholeCoreBindings (WholeCoreBindings(..), emptyIfaceForeign)
import GHC.Unit.Types (Module, UnitId)
import GHC.Utils.Binary
  ( Binary(..), openBinMem, unsafeUnpackBinBuffer, withBinBuffer )
import GHC.Utils.Outputable (text, pprCode)
import Tidepool.ArtifactBytes (ArtifactBytes, captureArtifactBytes)
import Tidepool.ExtractUtil (trySynchronous)
import Tidepool.FinalizedModule (FinalizedModule(..))

-- | These facts cannot be reconstructed from Core or a skinny interface.
-- Version 1 refuses them explicitly instead of emitting an incomplete input.
data UnsupportedCoreMetadata
  = ForeignExportStubs
  | ForeignSourceFiles
  | StaticPointerEntries
  | CostCentres
  | ModuleBreakpoints
  deriving (Eq, Show)

data FinalizedCoreFailure
  = FinalizedCoreUnsupported UnsupportedCoreMetadata
  | FinalizedCoreOwnerMismatch
  | FinalizedCoreInterfaceMismatch
  | FinalizedCoreCompilerMismatch
  | FinalizedCoreFormatMismatch
  | FinalizedCoreInterfaceBindingMissing String
  | FinalizedCoreTyConMissing String
  | FinalizedCoreTooLarge
  | FinalizedCoreEncodeFailure String
  | FinalizedCoreDecodeFailure String
  deriving (Eq, Show)

instance Exception FinalizedCoreFailure

isUnsupportedFinalizedCore :: FinalizedCoreFailure -> Bool
isUnsupportedFinalizedCore FinalizedCoreUnsupported{} = True
isUnsupportedFinalizedCore _ = False

-- | A native binary payload, using GHC's Name, FastString and IfaceType
-- tables. Types and coercions never pass through pretty printing or parsing.
-- The metadata tag certifies that every refused CgGuts field was empty at
-- capture, so decoding does not silently default discarded semantic facts.
data FinalizedCore = FinalizedCore
  { coreMagic :: String
  , coreVersion :: Word8
  , coreCompiler :: String
  , coreEmptyMetadata :: Word8
  , coreOwner :: Module
  , coreInterfaceHash :: Fingerprint
  , coreBindings :: [IfaceBindingX IfaceMaybeRhs IfaceTopBndrInfo]
  , coreTyCons :: [Name]
  , corePackages :: [UnitId]
  }

instance Binary FinalizedCore where
  put_ handle core = do
    put_ handle (coreMagic core)
    put_ handle (coreVersion core)
    put_ handle (coreCompiler core)
    put_ handle (coreOwner core)
    put_ handle (coreInterfaceHash core)
    -- No foreign stubs/files, static-pointer entries, cost centres or breaks.
    put_ handle (coreEmptyMetadata core)
    put_ handle (coreBindings core)
    put_ handle (coreTyCons core)
    put_ handle (corePackages core)
  get handle = do
    magic <- get handle
    version <- get handle
    compiler <- get handle
    owner <- get handle
    interfaceHash <- get handle
    metadata <- get handle
    FinalizedCore magic version compiler metadata owner interfaceHash
      <$> get handle <*> get handle <*> get handle

finalizedCoreLimit :: Int
finalizedCoreLimit = 32 * 1024 * 1024

-- | Capture only the exact finalized pair. The directory argument matches
-- interface capture callers; this format writes no temporary source or files.
captureFinalizedCore :: HscEnv -> FinalizedModule -> FilePath
  -> IO (Either FinalizedCoreFailure ArtifactBytes)
captureFinalizedCore env finalized _directory = case validateFinalized env finalized of
  Left failure -> pure (Left failure)
  Right () -> do
    encoded <- trySynchronous $ do
      handle <- openBinMem 4096
      putWithUserData QuietBinIFace NormalCompression handle payload
      withBinBuffer handle (pure . BS.copy)
    pure $ case encoded of
      Left failure -> Left (FinalizedCoreEncodeFailure (displayException failure))
      Right bytes
        | BS.length bytes > finalizedCoreLimit -> Left FinalizedCoreTooLarge
        | otherwise -> Right (captureArtifactBytes bytes)
  where
    guts = finalizedTidyGuts finalized
    interface = hm_iface (finalizedHomeModInfo finalized)
    payload = FinalizedCore
      { coreMagic = "TPFINALCORE"
      , coreVersion = 1
      , coreCompiler = cProjectVersion
      , coreEmptyMetadata = 0
      , coreOwner = cg_module guts
      , coreInterfaceHash = mi_iface_hash (mi_final_exts interface)
      , coreBindings = map toIfaceTopBind (cg_binds guts)
      , coreTyCons = map tyConName (cg_tycons guts)
      , corePackages = Set.toAscList (cg_dep_pkgs guts)
      }

validateFinalized :: HscEnv -> FinalizedModule -> Either FinalizedCoreFailure ()
validateFinalized env finalized
  | mi_module (hm_iface home) /= cg_module guts = Left FinalizedCoreOwnerMismatch
  | not (emptyForeignStubs (cg_foreign guts)) = unsupported ForeignExportStubs
  | not (null (cg_foreign_files guts)) = unsupported ForeignSourceFiles
  | not (null (cg_spt_entries guts)) = unsupported StaticPointerEntries
  | not (null (cg_ccs guts)) = unsupported CostCentres
  | isJust (cg_modBreaks guts) = unsupported ModuleBreakpoints
  | missing : _ <- missingGlobals = Left (FinalizedCoreInterfaceBindingMissing (label missing))
  | otherwise = () <$ traverse (resolveTyCon home . tyConName) (cg_tycons guts)
  where
    home = finalizedHomeModInfo finalized
    guts = finalizedTidyGuts finalized
    unsupported = Left . FinalizedCoreUnsupported
    -- GHC's tidy result can retain the monoidal empty ForeignStubs value.
    -- Use the same code rendering as its stub writer and preserve every
    -- initializer/finalizer obligation even when both documents are empty.
    emptyForeignStubs NoStubs = True
    emptyForeignStubs (ForeignStubs (CHeader header) (CStub body initializers finalizers)) =
      null (showSDoc (hsc_dflags env) (pprCode header))
        && null (showSDoc (hsc_dflags env) (pprCode body))
        && null initializers && null finalizers
    missingGlobals =
      [ name | binder <- bindersOfBinds (cg_binds guts)
      , let name = varName binder
      , isExternalName name
      , case lookupTypeEnv (md_types (hm_details home)) name of
          Just AnId{} -> False
          _ -> True ]

-- | Decode against the already admitted and hydrated skinny interface. The
-- caller owns its dependency HPT and the source-free module location. GHC's
-- whole-Core decoder ties local identities into the original declaration
-- environment; it performs neither source compilation nor Template Haskell.
decodeFinalizedCore :: HscEnv -> HomeModInfo -> ModLocation -> BS.ByteString
  -> IO (Either FinalizedCoreFailure FinalizedModule)
decodeFinalizedCore env home location bytes = do
  admitted <- readFinalizedCore env home bytes
  case admitted of
    Left failure -> pure (Left failure)
    Right (core, tycons) -> do
      reconstructed <- trySynchronous $ do
        types <- newIORef (md_types (hm_details home))
        let knotted = env { hsc_type_env_vars = knotVarsFromModuleEnv
              (mkModuleEnv [(coreOwner core, types)]) }
            whole = WholeCoreBindings (coreBindings core) (coreOwner core)
              location emptyIfaceForeign
        bindings <- initIfaceCheck (text "tidepool finalized Core") knotted
          (typecheckWholeCoreBindings types whole)
        pure (FinalizedModule home CgGuts
          { cg_module = coreOwner core
          , cg_tycons = tycons
          , cg_binds = bindings
          , cg_ccs = []
          , cg_foreign = NoStubs
          , cg_foreign_files = []
          , cg_dep_pkgs = Set.fromList (corePackages core)
          , cg_modBreaks = Nothing
          , cg_spt_entries = []
          })
      pure $ case reconstructed of
        Left failure -> Left (FinalizedCoreDecodeFailure (displayException failure))
        Right finalized -> Right finalized

-- The demanding owner must verify the companion's certified SHA before this
-- operation. Interface fingerprints deliberately exclude defining Core.
-- GHC make can hydrate these native bindings under its dependency HPT without
-- another frontend or an intermediate Core decode/re-encode.
attachFinalizedCore :: HscEnv -> HomeModInfo -> BS.ByteString
  -> IO (Either FinalizedCoreFailure ModIface)
attachFinalizedCore env home bytes = fmap
  (fmap (\(core, _) -> set_mi_extra_decls (Just (coreBindings core)) (hm_iface home)))
  (readFinalizedCore env home bytes)

readFinalizedCore :: HscEnv -> HomeModInfo -> BS.ByteString
  -> IO (Either FinalizedCoreFailure (FinalizedCore, [TyCon]))
readFinalizedCore env home bytes
  | BS.length bytes > finalizedCoreLimit = pure (Left FinalizedCoreTooLarge)
  | otherwise = do
      decoded <- trySynchronous $ do
        handle <- unsafeUnpackBinBuffer bytes
        core <- getWithUserData (hsc_NC env) handle
        pure $ if coreMagic core /= "TPFINALCORE" || coreVersion core /= 1
            || coreEmptyMetadata core /= 0
          then Left FinalizedCoreFormatMismatch
          else if coreCompiler core /= cProjectVersion
            then Left FinalizedCoreCompilerMismatch
          else if coreOwner core /= mi_module (hm_iface home)
            then Left FinalizedCoreOwnerMismatch
          else if coreInterfaceHash core /= mi_iface_hash (mi_final_exts (hm_iface home))
            then Left FinalizedCoreInterfaceMismatch
          else (core,) <$> traverse (resolveTyCon home) (coreTyCons core)
      pure $ case decoded of
        Left failure -> Left (FinalizedCoreDecodeFailure (displayException failure))
        Right result -> result

resolveTyCon :: HomeModInfo -> Name -> Either FinalizedCoreFailure TyCon
resolveTyCon home name = case lookupTypeEnv (md_types (hm_details home)) name of
  Just (ATyCon tycon) -> Right tycon
  _ -> Left (FinalizedCoreTyConMissing (label name))

label :: Name -> String
label = occNameString . nameOccName

-- | Local site evidence comes from the same admitted original interface,
-- including its native class-instance identities, rather than an empty list.
finalizedCoreSiteInstances :: FinalizedModule -> [ClsInst]
finalizedCoreSiteInstances = instEnvElts . md_insts . hm_details . finalizedHomeModInfo
