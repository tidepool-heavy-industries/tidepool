-- | Compiler plugins can read files, environment, or compiler state that is
-- absent from the source dependency graph. Only the retained-unfoldings
-- option is installed by this pipeline itself and has an explicit
-- per-module recompilation fingerprint.
module Tidepool.CompileInputPolicy
  ( pluginInputIssues
  , pluginInputIssuesFor
  ) where

import GHC.Driver.Session (DynFlags(..))
import GHC.Unit.Module (ModuleName, mkModuleName, moduleNameString)

-- | Describe compiler plugins that read inputs not covered by source
-- fingerprints. The diagnostics are stable so callers can use them as the
-- reason for declining cached products.
pluginInputIssues :: DynFlags -> [String]
pluginInputIssues flags = pluginInputIssuesFor
  (pluginModNames flags)
  (pluginModNameOpts flags)

-- | Pure form over the plugin flags as recorded by GHC. Dynamic plugin
-- loading is always untracked. Options for other plugins are also untracked;
-- the sole exception is the option emitted by the retained-unfoldings owner
-- to key its own recompilation fingerprint.
pluginInputIssuesFor :: [ModuleName] -> [(ModuleName, String)] -> [String]
pluginInputIssuesFor dynamicPlugins pluginOptions =
  [ "dynamic-plugin=" ++ moduleNameString plugin
  | plugin <- dynamicPlugins
  ] ++
  [ "plugin-option=" ++ moduleNameString owner ++ ":" ++ option
  | (owner, option) <- pluginOptions
  , owner /= retainedUnfoldingsPlugin
  ]

retainedUnfoldingsPlugin :: ModuleName
retainedUnfoldingsPlugin = mkModuleName "Tidepool.RetainedUnfoldings"
