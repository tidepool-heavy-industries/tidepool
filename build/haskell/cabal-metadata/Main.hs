module Main (main) where

import Control.Monad (unless)
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as LBS
import Data.Foldable (toList)
import Data.List (sortOn)
import Distribution.Compiler
  ( CompilerFlavor(GHC), AbiTag(NoAbiTag), buildCompilerId, unknownCompilerInfo
  , perCompilerFlavorToList )
import Distribution.Package (pkgName, pkgVersion)
import Distribution.PackageDescription
import Distribution.PackageDescription.Configuration (finalizePD)
import Distribution.PackageDescription.Parsec
  ( parseGenericPackageDescription, runParseResult )
import Distribution.Pretty (prettyShow)
import Distribution.System (buildPlatform)
import Distribution.Types.ComponentRequestedSpec (ComponentRequestedSpec(..))
import Distribution.Types.Dependency (depLibraries, depPkgName, depVerRange)
import Distribution.Types.ExeDependency (ExeDependency(..))
import Distribution.Types.Flag
  ( FlagAssignment, flagDefault, flagName, mkFlagAssignment, unFlagAssignment
  , unFlagName )
import Distribution.Types.LibraryName (LibraryName(..))
import Distribution.Utils.Json (Json(..), renderJson)
import Distribution.Utils.Path (getSymbolicPath)
import System.Environment (getArgs)
import System.Exit (die)

-- Cabal resolves syntax, common imports, defaults and conditional semantics.
-- Registration requests every supported role explicitly; Buck target selection
-- decides which of those components is built or executed.
data Phase = Production | Tests | Benchmarks deriving (Eq, Show)

phaseName :: Phase -> String
phaseName Production = "production"
phaseName Tests = "tests"
phaseName Benchmarks = "benchmarks"

flagAssignment :: Phase -> GenericPackageDescription -> FlagAssignment
flagAssignment phase description = mkFlagAssignment
  [ (flagName flag, selected flag) | flag <- genPackageFlags description ]
  where
    selected flag = case unFlagName (flagName flag) of
      "test-tools" -> phase /= Production
      "benchmarks" -> phase == Benchmarks
      -- Future unrelated flags retain Cabal's declared default explicitly.
      _ -> flagDefault flag

strings :: [String] -> Json
strings = JsonArray . map JsonString

libraryName :: LibraryName -> Json
libraryName LMainLib = JsonNull
libraryName (LSubLibName name) = JsonString (prettyShow name)

dependencyJson :: Dependency -> Json
dependencyJson dependency = JsonObject
  [ ("package", JsonString (prettyShow (depPkgName dependency)))
  , ("version_range", JsonString (prettyShow (depVerRange dependency)))
  , ("libraries", JsonArray (map libraryName (toList (depLibraries dependency))))
  ]

toolJson :: ExeDependency -> Json
toolJson (ExeDependency packageName component versionRange) = JsonObject
  [ ("package", JsonString (prettyShow packageName))
  , ("component", JsonString (prettyShow component))
  , ("version_range", JsonString (prettyShow versionRange))
  ]

-- These fields require real native actions/edges, rather than compiler strings
-- or ambient preprocessors. Reject unsupported inputs at their owning boundary.
unsupportedInputs :: BuildInfo -> [String]
unsupportedInputs info =
  [ field | (field, present) <-
    [ ("build-tools", not (null (buildTools info)))
    , ("cpp-options", not (null (cppOptions info)))
    , ("asm-options", not (null (asmOptions info)))
    , ("cmm-options", not (null (cmmOptions info)))
    , ("cc-options", not (null (ccOptions info)))
    , ("cxx-options", not (null (cxxOptions info)))
    , ("ld-options", not (null (ldOptions info)))
    , ("hsc2hs-options", not (null (hsc2hsOptions info)))
    , ("pkgconfig-depends", not (null (pkgconfigDepends info)))
    , ("frameworks", not (null (frameworks info)))
    , ("extra-framework-dirs", not (null (extraFrameworkDirs info)))
    , ("asm-sources", not (null (asmSources info)))
    , ("cmm-sources", not (null (cmmSources info)))
    , ("c-sources", not (null (cSources info)))
    , ("cxx-sources", not (null (cxxSources info)))
    , ("js-sources", not (null (jsSources info)))
    , ("virtual-modules", not (null (virtualModules info)))
    , ("autogen-modules", not (null (autogenModules info)))
    , ("extra-libraries", not (null (extraLibs info)))
    , ("extra-libraries-static", not (null (extraLibsStatic info)))
    , ("extra-ghci-libraries", not (null (extraGHCiLibs info)))
    , ("extra-bundled-libraries", not (null (extraBundledLibs info)))
    , ("extra-lib-flavours", not (null (extraLibFlavours info)))
    , ("extra-dynamic-library-flavours", not (null (extraDynLibFlavours info)))
    , ("extra-lib-dirs", not (null (extraLibDirs info)))
    , ("extra-lib-dirs-static", not (null (extraLibDirsStatic info)))
    , ("include-dirs", not (null (includeDirs info)))
    , ("includes", not (null (includes info)))
    , ("autogen-includes", not (null (autogenIncludes info)))
    , ("install-includes", not (null (installIncludes info)))
    , ("ghc-prof-options", any (not . null . snd) (perCompilerFlavorToList (profOptions info)))
    , ("ghc-shared-options", any (not . null . snd) (perCompilerFlavorToList (sharedOptions info)))
    , ("ghc-prof-shared-options", any (not . null . snd) (perCompilerFlavorToList (profSharedOptions info)))
    , ("ghc-static-options", any (not . null . snd) (perCompilerFlavorToList (staticOptions info)))
    , ("non-GHC compiler options", any (\(flavor, opts) -> flavor /= GHC && not (null opts)) (perCompilerFlavorToList (options info)))
    , ("mixins", not (null (mixins info)))
    , ("custom build fields", not (null (customFieldsBI info)))
    ], present ]

componentJson :: Phase -> String -> String -> Maybe FilePath -> [String] -> BuildInfo -> IO Json
componentJson phase kind name mainSource exposed info = do
  unless (null (unsupportedInputs info)) $
    die (name ++ ": unsupported native Cabal build inputs: " ++ show (unsupportedInputs info))
  pure $ JsonObject
    [ ("phase", JsonString (phaseName phase))
    , ("kind", JsonString kind)
    , ("name", JsonString name)
    , ("main", maybe JsonNull JsonString mainSource)
    , ("source_dirs", strings (map getSymbolicPath (hsSourceDirs info)))
    , ("modules", strings (exposed ++ map prettyShow (otherModules info)))
    , ("language", maybe (JsonString "Haskell98") (JsonString . prettyShow) (defaultLanguage info))
    , ("extensions", strings (map prettyShow (defaultExtensions info ++ oldExtensions info)))
    , ("ghc_options", strings (hcOptions GHC info))
    , ("dependencies", JsonArray (map dependencyJson (targetBuildDepends info)))
    , ("tools", JsonArray (map toolJson (buildToolDepends info)))
    ]

componentsJson :: Phase -> PackageDescription -> IO [Json]
componentsJson phase description = do
  unless (buildType description == Simple && setupBuildInfo description == Nothing) $
    die "native Cabal components require build-type Simple without custom setup"
  unless (null (foreignLibs description) && null (dataFiles description)) $
    die "foreign libraries and package data files need declared native implementations"
  libraries <- mapM libraryJson (filter (buildable . libBuildInfo) (allLibraries description))
  binaries <- mapM executableJson (filter (buildable . buildInfo) (executables description))
  suites <- if phase == Tests
    then mapM testJson (filter (buildable . testBuildInfo) (testSuites description))
    else pure []
  unless (null (benchmarks description)) $
    die "Cabal benchmark interfaces need a native implementation; compiler measurements use explicit executables"
  pure (libraries ++ binaries ++ suites)
  where
    libraryJson component = do
      unless (null (reexportedModules component) && null (signatures component)) $
        die (prettyShow (libName component) ++ ": module reexports/signatures need native Backpack support")
      let name = case libName component of
            LMainLib -> prettyShow (pkgName (package description))
            LSubLibName value -> prettyShow value
      componentJson phase "library" name Nothing (map prettyShow (exposedModules component)) (libBuildInfo component)
    executableJson component = componentJson phase "executable" (prettyShow (exeName component))
      (Just (getSymbolicPath (modulePath component))) [] (buildInfo component)
    testJson component = do
      unless (null (testCodeGenerators component)) $
        die (prettyShow (testName component) ++ ": test code generators need declared native producers")
      case testInterface component of
        TestSuiteExeV10 _ path -> componentJson phase "test-suite" (prettyShow (testName component))
          (Just (getSymbolicPath path)) [] (testBuildInfo component)
        _ -> die (prettyShow (testName component) ++ ": unsupported native test interface")

finalize :: GenericPackageDescription -> Phase -> IO Json
finalize generic phase = do
  let requested = ComponentRequestedSpec (phase == Tests) False
      assignment = flagAssignment phase generic
      identity = unknownCompilerInfo buildCompilerId NoAbiTag
  -- Every package flag is assigned explicitly. This resolves conditions without
  -- a dependency solver; native Buck/Nix edges own actual package availability.
  (description, resolvedFlags) <- either
    (die . ("Cabal finalization failed: " ++) . show) pure $
    finalizePD assignment requested (const True) buildPlatform identity [] generic
  components <- componentsJson phase description
  pure $ JsonObject
    [ ("phase", JsonString (phaseName phase))
    , ("flags", JsonObject [(unFlagName name, JsonBool value) | (name, value) <- sortOn (unFlagName . fst) (unFlagAssignment resolvedFlags)])
    , ("components", JsonArray components)
    ]

main :: IO ()
main = do
  path <- getArgs >>= \arguments -> case arguments of
    [value] -> pure value
    _ -> die "usage: cabal-metadata PACKAGE.cabal"
  bytes <- BS.readFile path
  let (warnings, parsed) = runParseResult (parseGenericPackageDescription bytes)
  -- Cabal warns when it ignores unknown fields/sections. Treat all parser
  -- warnings as errors here so unsupported syntax never silently changes a graph.
  unless (null warnings) (die (path ++ ": Cabal parse warnings: " ++ show warnings))
  generic <- either (\failure -> die (path ++ ": Cabal parse failure: " ++ show failure)) pure parsed
  phases <- mapM (finalize generic) [Production, Tests, Benchmarks]
  let description = packageDescription generic
  LBS.putStr $ renderJson $ JsonObject
    [ ("schema", JsonNumber 1)
    , ("compiler", JsonString (prettyShow buildCompilerId))
    , ("platform", JsonString (prettyShow buildPlatform))
    , ("package", JsonString (prettyShow (pkgName (package description))))
    , ("version", JsonString (prettyShow (pkgVersion (package description))))
    , ("configurations", JsonArray phases)
    ]
