module Main (main) where

import Control.Exception (SomeException, bracket, try)
import Control.Monad (unless)
import Data.List (isInfixOf, sort)
import GHC (moduleNameString)
import GHC.Builtin.Types (boolTy)
import GHC.Unit.Types (moduleName)
import System.Directory
  ( createDirectoryIfMissing, getTemporaryDirectory, removePathForcibly )
import System.FilePath ((</>))
import Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult(..), CompilePurpose(..)
  , runPipeline, runPipelineSelected, withResidentPipelineSelected )
import Tidepool.PreparedStg (PreparedModule(..))
import Tidepool.EffectSchema (SiteType(..), YieldSite(..), sitedVerbs, vsName)
import Tidepool.ExecutionIR
  ( LiteralInventory(..), PreparedFact(..), PreparedInventory(..), PreparedSupport(..), inventoryPreparedModule
  , renderPreparedInventory )
import Tidepool.PreparedSites (buildYieldSite)

assert :: Bool -> String -> IO ()
assert ok message = unless ok (ioError (userError message))

preparedShape :: PreparedPipelineResult -> [(String, Int)]
preparedShape = sort . map shape . pprModules
  where
    shape prepared =
      ( moduleNameString (moduleName (pmModule prepared))
      , length (pmBindings prepared)
      )

allPreparedEvidence :: PreparedPipelineResult -> [String]
allPreparedEvidence = sort . map (renderPreparedInventory . inventoryPreparedModule) . pprModules

assertProductionFacts :: PreparedPipelineResult -> IO ()
assertProductionFacts result = do
  let facts = concatMap (foldr (:) [] . inventoryFacts . inventoryPreparedModule) (pprModules result)
      literals = concatMap (foldr (:) [] . inventoryLiterals . inventoryPreparedModule) (pprModules result)
  assert (any isImport facts) "prepared inventory omitted imported identities"
  assert (any isOperation facts) "prepared inventory omitted operation signatures"
  assert (any isTagCount facts) "prepared inventory omitted tag-inference evidence"
  assert (any isConstructor facts) "prepared inventory omitted constructor layouts"
  assert (any isNulPolicy facts) "prepared inventory omitted the explicit embedded-NUL support decision"
  assert (any isNulLiteral literals) "prepared inventory did not retain the embedded-NUL bytes"
 where
  isImport ImportedValue{} = True; isImport _ = False
  isOperation OperationSignature{} = True; isOperation _ = False
  isTagCount TagInferenceCount{} = True; isTagCount _ = False
  isConstructor ConstructorLayout{} = True; isConstructor _ = False
  isNulPolicy (SupportStatus EmbeddedNulStringsRetainedAsModifiedUtf8) = True; isNulPolicy _ = False
  isNulLiteral (StringLiteral bytes _) = 0 `elem` bytes || [0xC0, 0x80] `isInfixOf` bytes
  isNulLiteral _ = False

preparedEvidence :: String -> PreparedPipelineResult -> (String, [YieldSite])
preparedEvidence wanted result = case filter isWanted (pprModules result) of
  [prepared] ->
    (renderPreparedInventory (inventoryPreparedModule prepared), pmYieldSites prepared)
  modules -> error ("expected one prepared module " ++ wanted ++ ", got "
    ++ show (map (moduleNameString . moduleName . pmModule) modules))
  where
    isWanted = (== wanted) . moduleNameString . moduleName . pmModule

expectFailure :: String -> IO result -> IO ()
expectFailure label action = do
  outcome <- try (action >> pure ()) :: IO (Either SomeException ())
  case outcome of
    Left _ -> pure ()
    Right _ -> ioError (userError (label ++ ": expected compiler failure"))

expectFailureContaining :: String -> String -> IO result -> IO ()
expectFailureContaining label needle action = do
  outcome <- try (action >> pure ()) :: IO (Either SomeException ())
  case outcome of
    Left failure -> assert (needle `isInfixOf` show failure)
      (label ++ ": unexpected failure: " ++ show failure)
    Right _ -> ioError (userError (label ++ ": expected compiler failure"))

main :: IO ()
main = do
  tmp <- getTemporaryDirectory
  let work = tmp </> "tidepool-prepared-stg-pipeline-test"
  bracket
    (removePathForcibly work >> createDirectoryIfMissing True work >> pure work)
    removePathForcibly
    $ \dir -> do
      let dep = dir </> "Dep.hs"
          effectsDir = dir </> "Tidepool" </> "Effects"
          effects = effectsDir </> "Core.hs"
          unfoldDir = dir </> "Tidepool" </> "Actors"
          unfold = unfoldDir </> "Unfold.hs"
          siteTarget = dir </> "SiteExpr.hs"
          malformedSiteTarget = dir </> "MalformedSiteExpr.hs"
          target = dir </> "Expr.hs"
          validTarget = unlines
            [ "module Expr where"
            , "import Dep"
            , "data Packed = Packed !Int !Char"
            , "embeddedNul :: String"
            , "embeddedNul = \"left\\0right\""
            , "result :: Int"
            , "result = case Packed 41 'x' of Packed n _ -> helper n + length embeddedNul"
            ]
      createDirectoryIfMissing True effectsDir
      createDirectoryIfMissing True unfoldDir
      writeFile dep (unlines
        [ "module Dep where"
        , "helper :: Int -> Int"
        , "helper x = x + 1"
        ])
      writeFile effects (unlines
        [ "{-# LANGUAGE ExplicitForAll #-}"
        , "{-# LANGUAGE TypeApplications #-}"
        , "module Tidepool.Effects.Core where"
        , "{-# OPAQUE runLLMTurn #-}"
        , "runLLMTurn :: forall a. String -> Maybe a"
        , "runLLMTurn _ = Nothing"
        , "runLLMTurnSited :: forall a. Int -> String -> Maybe a"
        , "runLLMTurnSited _ _ = Nothing"
        , "forkAllSited :: forall a. Int -> String -> Maybe [a]"
        , "forkAllSited _ _ = Nothing"
        , "keepForkAllSited :: Maybe [Bool]"
        , "keepForkAllSited = forkAllSited @Bool 0 \"\""
        ])
      writeFile unfold (unlines
        [ "{-# LANGUAGE ExplicitForAll #-}"
        , "module Tidepool.Actors.Unfold where"
        , "child :: forall result child effects input. input -> Maybe result"
        , "child _ = Nothing"
        , "childSited :: forall result child effects input. Int -> input -> Maybe result"
        , "childSited _ _ = Nothing"
        ])
      writeFile target validTarget
      writeFile siteTarget (unlines
        [ "{-# LANGUAGE TypeApplications #-}"
        , "module SiteExpr where"
        , "import Tidepool.Effects.Core"
        , "typedSite :: Maybe Bool"
        , "typedSite = runLLMTurn @Bool \"prepared\""
        ])
      writeFile malformedSiteTarget (unlines
        [ "{-# LANGUAGE TypeApplications #-}"
        , "module MalformedSiteExpr where"
        , "import Tidepool.Actors.Unfold"
        , "malformedSite :: String -> Maybe Bool"
        , "malformedSite = child @Bool @Int @Char @String"
        ])

      _legacy <- runPipeline target [dir]

      direct <- runPipelineSelected PreparedStg target [dir]
      assertProductionFacts direct
      let directShape = preparedShape direct
      assert (map fst directShape == ["Dep", "Expr"])
        ("direct prepared modules lost context: " ++ show directShape)
      siteDirect <- runPipelineSelected PreparedStg siteTarget [dir]
      let (siteInventory, directSites) = preparedEvidence "SiteExpr" siteDirect
      assert (case directSites of
                [site] -> "runLLMTurnSited" `isInfixOf` siteInventory
                  && show (ysSite site) `isInfixOf` siteInventory
                _ -> False)
        "typed site was not elaborated before preparation"
      expectFailureContaining "direct malformed recognized site" "is not fully applied" $
        runPipelineSelected PreparedStg malformedSiteTarget [dir]
      let forkAllSpec = case filter ((== "forkAll") . vsName) sitedVerbs of
            [spec] -> spec
            _ -> error "missing forkAll VerbSpec"
          listSite = buildYieldSite forkAllSpec "ListSiteExpr.listSite" 0 boolTy []
      assert (stType (ysAnswer listSite) == "[Bool]")
        "list-answer site did not preserve shared legacy answer identity"

      withResidentPipelineSelected [dir] $ \compileSite -> do
        siteCold <- compileSite PreparedStg GeneralCompile Nothing siteTarget [] Nothing
        siteWarm <- compileSite PreparedStg GeneralCompile Nothing siteTarget [] Nothing
        assert (preparedEvidence "SiteExpr" siteCold == (siteInventory, directSites))
          "resident cold typed-site evidence differs from direct"
        assert (preparedEvidence "SiteExpr" siteWarm == (siteInventory, directSites))
          "resident warm typed-site evidence differs from direct"
        expectFailureContaining "resident malformed recognized site" "is not fully applied" $
          compileSite PreparedStg GeneralCompile Nothing malformedSiteTarget [] Nothing
        siteRecovered <- compileSite PreparedStg GeneralCompile Nothing siteTarget [] Nothing
        assert (preparedEvidence "SiteExpr" siteRecovered == (siteInventory, directSites))
          "resident compiler did not recover after malformed recognized site"

      writeFile target "module Expr where\nresult =\n"
      expectFailure "direct prepared" $
        runPipelineSelected PreparedStg target [dir]
      writeFile target validTarget

      withResidentPipelineSelected [dir] $ \compile -> do
        _coldLegacy <- compile LegacyCore GeneralCompile Nothing target [] Nothing

        cold <- compile PreparedStg GeneralCompile Nothing target [] Nothing
        warm <- compile PreparedStg GeneralCompile Nothing target [] Nothing
        assert (preparedShape cold == directShape)
          "resident cold prepared output differs from direct output"
        assert (preparedShape warm == directShape)
          ("resident warm prepared output did not retain module results: "
            ++ show (preparedShape warm) ++ " /= " ++ show directShape)
        assert (allPreparedEvidence cold == allPreparedEvidence direct)
          "resident cold prepared facts differ from direct output"
        assert (allPreparedEvidence warm == allPreparedEvidence direct)
          "resident warm prepared facts differ from direct output"

        _warmLegacy <- compile LegacyCore GeneralCompile Nothing target [] Nothing

        writeFile target "module Expr where\nresult =\n"
        expectFailure "resident prepared" $
          compile PreparedStg GeneralCompile Nothing target [] Nothing
        writeFile target validTarget
        recovered <- compile PreparedStg GeneralCompile Nothing target [] Nothing
        assert (preparedShape recovered == directShape)
          "resident compiler did not recover after a request-local failure"
