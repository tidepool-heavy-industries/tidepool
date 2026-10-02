-- | Compiler plugins can read files, environment, or compiler state that is
-- absent from the source dependency graph. Only the retained-unfoldings
-- option is installed by this pipeline itself and has an explicit
-- per-module recompilation fingerprint.
module Tidepool.CompileInputPolicy
  ( pluginInputIssues
  , pluginInputIssuesFor
  ) where

import GHC.Driver.Session (DynFlags(..))
import GHC.Driver.Plugins.External (ExternalPluginSpec(..))
import GHC.Unit.Module (ModuleName, mkModuleName, moduleNameString)

-- | Describe compiler plugins that read inputs not covered by source
-- fingerprints. The diagnostics are stable so callers can use them as the
-- reason for declining cached products.
pluginInputIssues :: DynFlags -> [String]
pluginInputIssues flags = pluginInputIssuesFor
  (pluginModNames flags)
  (pluginModNameOpts flags)
  (externalPluginSpecs flags)

-- | Pure form over the plugin flags as recorded by GHC. Dynamic plugin
-- and external library loading are always untracked. Options for other plugins are also untracked;
-- the sole exception is the option emitted by the retained-unfoldings owner
-- to key its own recompilation fingerprint.
pluginInputIssuesFor :: [ModuleName] -> [(ModuleName, String)] -> [ExternalPluginSpec] -> [String]
pluginInputIssuesFor dynamicPlugins pluginOptions externalPlugins =
  [ "dynamic-plugin=" ++ moduleNameString plugin
  | plugin <- dynamicPlugins
  ] ++
  [ "plugin-option=" ++ moduleNameString owner ++ ":" ++ option
  | (owner, option) <- pluginOptions
  , owner /= retainedUnfoldingsPlugin
  ] ++
  [ "external-plugin=" ++ esp_module plugin
  | plugin <- externalPlugins
  ]

retainedUnfoldingsPlugin :: ModuleName
retainedUnfoldingsPlugin = mkModuleName "Tidepool.RetainedUnfoldings"
