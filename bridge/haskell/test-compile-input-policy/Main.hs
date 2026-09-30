module Main (main) where

import Control.Monad (unless)
import GHC.Unit.Module (mkModuleName)
import System.Exit (exitFailure)
import Tidepool.CompileInputPolicy (pluginInputIssuesFor)

main :: IO ()
main = do
  check "no plugin flags stay eligible" []
    (pluginInputIssuesFor [] [])
  check "the owning static retained plugin option stays eligible" []
    (pluginInputIssuesFor []
      [(mkModuleName "Tidepool.RetainedUnfoldings", "(\"main\",\"Example\")")])
  check "dynamic plugin loading is untracked"
    ["dynamic-plugin=Vendor.ReadsFiles"]
    (pluginInputIssuesFor [mkModuleName "Vendor.ReadsFiles"] [])
  check "foreign plugin options are untracked"
    ["plugin-option=Vendor.Plugin:read-schema"]
    (pluginInputIssuesFor [] [(mkModuleName "Vendor.Plugin", "read-schema")])
  check "a dynamic load of the owning plugin is still untracked"
    ["dynamic-plugin=Tidepool.RetainedUnfoldings"]
    (pluginInputIssuesFor [mkModuleName "Tidepool.RetainedUnfoldings"] [])
  check "dynamic plugins and foreign options both refuse reuse"
    [ "dynamic-plugin=Vendor.ReadsFiles"
    , "plugin-option=Vendor.Plugin:reads-env"
    ]
    (pluginInputIssuesFor
      [mkModuleName "Vendor.ReadsFiles"]
      [(mkModuleName "Vendor.Plugin", "reads-env")])

check :: String -> [String] -> [String] -> IO ()
check label expected actual = unless (expected == actual) $ do
  putStrLn (label ++ ": expected " ++ show expected ++ ", got " ++ show actual)
  exitFailure
