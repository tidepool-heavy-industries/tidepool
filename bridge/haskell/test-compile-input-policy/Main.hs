module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import Control.Monad (unless)
import GHC.Driver.Plugins.External (ExternalPluginSpec(..))
import GHC.Unit.Module (mkModuleName)
import Tidepool.CompileInputPolicy (pluginInputIssuesFor)

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "compile-input-policy"
  [ testCase "no plugin flags stay eligible" $ check "no plugin flags stay eligible" []
    (pluginInputIssuesFor [] [] [])
  , testCase "the owning static retained plugin option stays eligible" $ check "the owning static retained plugin option stays eligible" []
    (pluginInputIssuesFor []
      [(mkModuleName "Tidepool.RetainedUnfoldings", "(\"main\",\"Example\")")] [])
  , testCase "dynamic plugin loading is untracked" $ check "dynamic plugin loading is untracked"
    ["dynamic-plugin=Vendor.ReadsFiles"]
    (pluginInputIssuesFor [mkModuleName "Vendor.ReadsFiles"] [] [])
  , testCase "foreign plugin options are untracked" $ check "foreign plugin options are untracked"
    ["plugin-option=Vendor.Plugin:read-schema"]
    (pluginInputIssuesFor [] [(mkModuleName "Vendor.Plugin", "read-schema")] [])
  , testCase "a dynamic load of the owning plugin is still untracked" $ check "a dynamic load of the owning plugin is still untracked"
    ["dynamic-plugin=Tidepool.RetainedUnfoldings"]
    (pluginInputIssuesFor [mkModuleName "Tidepool.RetainedUnfoldings"] [] [])
  , testCase "dynamic plugins and foreign options both refuse reuse" $ check "dynamic plugins and foreign options both refuse reuse"
    [ "dynamic-plugin=Vendor.ReadsFiles"
    , "plugin-option=Vendor.Plugin:reads-env"
    ]
    (pluginInputIssuesFor
      [mkModuleName "Vendor.ReadsFiles"]
      [(mkModuleName "Vendor.Plugin", "reads-env")] [])
  , testCase "external library plugins without ordinary plugin flags refuse reuse" $ check "external library plugins without ordinary plugin flags refuse reuse"
    ["external-plugin=Vendor.ReadsFiles"]
    (pluginInputIssuesFor [] []
      [ExternalPluginSpec "/not-loaded/plugin.so" "vendor" "Vendor.ReadsFiles" []])
  , testCase "an external library load of the owning plugin is still untracked" $ check "an external library load of the owning plugin is still untracked"
    ["external-plugin=Tidepool.RetainedUnfoldings"]
    (pluginInputIssuesFor [] []
      [ExternalPluginSpec "/not-loaded/plugin.so" "vendor" "Tidepool.RetainedUnfoldings" []])
  ]

check :: String -> [String] -> [String] -> IO ()
check label expected actual = unless (expected == actual) $ do
  fail (label ++ ": expected " ++ show expected ++ ", got " ++ show actual)
