-- | The compiler's fixed support operations belong to its pinned package DB.
-- Package qualification excludes authored home sources with the same name.
module Tidepool.ResumePackage (resolveResumeInterface) where

import GHC.Data.FastString (fsLit)
import GHC.Driver.Env (HscEnv, hsc_home_unit)
import GHC.Driver.Env.Types (hsc_unit_env)
import GHC.Types.PkgQual (PkgQual(OtherPkg))
import GHC.Unit.Env (ue_units)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Unit.Home (isHomeUnit)
import GHC.Unit.Info (PackageName(..))
import GHC.Unit.Module (mkModuleName, moduleUnit)
import GHC.Unit.Module.ModIface (ModIface, mi_module)
import GHC.Unit.State (lookupPackageName)
import Tidepool.FatIface (readExactInterface)

resolveResumeInterface :: HscEnv -> IO (Either String ModIface)
resolveResumeInterface environment = case lookupPackageName
    (ue_units (hsc_unit_env environment)) (PackageName (fsLit "tidepool-resume")) of
  Nothing -> pure (Left "compiler support package is absent from its pinned package DB")
  Just selected -> do
    found <- findImportedModule environment (mkModuleName "Tidepool.Internal.Resume") (OtherPkg selected)
    case found of
      Found _ owner
        | moduleUnit owner == selected
        , not (isHomeUnit (hsc_home_unit environment) (moduleUnit owner)) -> do
            loaded <- readExactInterface environment owner
            pure $ case loaded of
              Right (iface, _) | mi_module iface == owner -> Right iface
              Right _ -> Left "compiler support interface has another package owner"
              Left reason -> Left ("compiler support interface cannot be read: " ++ show reason)
      _ -> pure (Left "compiler support module does not resolve to its pinned package unit")
