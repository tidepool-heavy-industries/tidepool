module DemandScopeTest (demandScopeChecks) where

import Control.Exception (bracket)
import Control.Monad (forM_, unless, foldM)
import Control.Monad.IO.Class (liftIO)
import qualified Data.Map.Strict as Map
import qualified Data.ByteString as BS
import GHC
import GHC.Driver.Backend (noBackend)
import System.Directory
  ( createDirectory, getTemporaryDirectory, removeDirectoryRecursive, removeFile )
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import Tidepool.Binders (sourceModuleFlags)
import Tidepool.ExactHydration
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.TypedSegment.Source
import Tidepool.TypedSegment.Types

-- These exercise the production demand owner with ordinary data as bindings.
-- World/checked-value authority is exercised separately by the real service
-- consumer; lexical scope tests must not fabricate that authority.
demandScopeChecks :: IO ()
demandScopeChecks = do
  libdir <- getLibdir
  temporary <- getTemporaryDirectory
  let acquire = do
        (path,handle) <- openTempFile temporary "tidepool-demand-scope"
        hClose handle
        removeFile path
        createDirectory path
        pure path
  bracket acquire removeDirectoryRecursive $ \directory ->
    runGhc (Just libdir) $ do
      initial <- getSessionDynFlags
      forM_ controls $ \(name,root,aliases,expected) -> do
        source <- liftIO $ case name of
          "DeclarationModule" -> pure (unlines
            ["module DeclarationModule where", "data Box = Box Int", "refer = old"])
          "DeclarationQuote" -> pure (unlines
            ["{-# LANGUAGE QuasiQuotes #-}", "module DeclarationQuote where"
            ,"[doesNotExist| refuse before rename |]"])
          _ -> readFile ("test-cell-splitter/fixtures/demand-scope" </> name ++ ".hs")
        actual <- liftIO (sourceModuleFlags initial source)
          >>= either (liftIO . fail . show) pure
        _ <- setSessionDynFlags actual {backend=noBackend,ghcLink=NoLink}
        environment <- getSession
        liftIO $ do
          let path = directory </> name ++ ".hs"
              bindings = Map.fromList (zip aliases [0 :: Int ..])
          writeFile path source
          recipe <- generatedCheckingTemplateRecipe actual (CheckedTemplateImports [] [])
            source source path name >>= either fail pure
          captured <- captureGeneratedScaffoldSource recipe path >>= either fail pure
          scope <- newSegmentDemandScope environment bindings
          case prepareModuleDemand actual captured of
            Left _ | expected == ParseRefusal -> pure ()
            Left failure -> fail (name ++ ": parsing refused " ++ show failure)
            Right _ | expected == ParseRefusal -> fail (name ++ ": quote passed the parser gate")
            Right parsed -> do
              let sealedPath = capturedGeneratedScaffoldPath captured
                  sealedBytes = capturedGeneratedScaffoldBytes captured
              unless (moduleDemandMatchesSource parsed sealedPath sealedBytes
                  && not (moduleDemandMatchesSource parsed (sealedPath ++ ".other") sealedBytes)
                  && not (moduleDemandMatchesSource parsed sealedPath (sealedBytes <> BS.singleton 0)))
                (fail (name ++ ": quote proof lost its exact source identity"))
              demanded <- moduleInputOccurrences environment scope parsed
              if name == "ScopeControl" then do
                let makePlan checker = either (fail . show) pure
                      (typedSegmentPlan (replicate 64 '0') checker
                        [TypedItemPlan 0 "item" 1 (LetItem "marker" [])])
                plan <- makePlan root
                typed <- either (fail . show) pure (prepareSegmentDemand plan actual captured)
                typedDemand <- typedSegmentInputOccurrences environment scope typed
                unless (typedDemand == demanded
                    && moduleDemandMatchesSource (segmentModuleDemand typed) sealedPath sealedBytes
                    && segmentDemandMatchesSource typed sealedPath sealedBytes)
                  (fail "typed root wrapper changed its module proof or demand")
                wrong <- makePlan "missingTypedCheckerRoot"
                case prepareSegmentDemand wrong actual captured of
                  Left UnprovedRootAbstraction -> pure ()
                  _ -> fail "typed root wrapper admitted a plan without its unique checker root"
              else pure ()
              case (expected,demanded) of
                (Bindings names,Right selected) | map fst selected == names -> pure ()
                (RenameRefusal,Left (InputDemandRenameRejected diagnostic))
                  | not (null diagnostic) -> putStrLn (name ++ ": " ++ diagnostic)
                _ -> fail (name ++ ": wrong demand or refusal phase")
              _ <- foldM (\(previous,previousBindings) changes -> do
                updated <- updateSegmentDemandScope environment changes previous
                let nextBindings = Map.mapMaybe id (Map.union (Map.fromList changes)
                      (Map.map Just previousBindings))
                rebuilt <- newSegmentDemandScope environment nextBindings
                incrementalDemand <- moduleInputOccurrences environment updated parsed
                rebuiltDemand <- moduleInputOccurrences environment rebuilt parsed
                unless (incrementalDemand == rebuiltDemand)
                  (fail (name ++ ": alias delta differs from a fresh GHC scope"))
                pure (updated,nextBindings)) (scope,bindings)
                  [ [("old",Just 100)]
                  , [("id",Just 101),("old",Nothing)]
                  , [("old",Just 102),("pure",Just 103)]
                  , [("id",Nothing),("pure",Nothing),("old",Nothing)] ]
              forM_ demanded $ \selected -> forM_ selected $ \(alias,binding) ->
                unless (Map.lookup alias bindings == Just binding)
                  (fail (name ++ ": lost exact alias/binding association"))
          writeFile path (source ++ "\nchanged = ()\n")
          stale <- captureGeneratedScaffoldSource recipe path
          case stale of
            Left _ -> pure ()
            Right _ -> fail (name ++ ": source drift was accepted")
      liftIO (putStrLn ("demand scope: " ++ show (length controls) ++ " controls passed"))

data ExpectedDemand = Bindings [String] | ParseRefusal | RenameRefusal
  deriving (Eq, Show)

controls :: [(String,String,[String],ExpectedDemand)]
controls =
  [ ("ScopeControl","__tidepool_segment_check",["old","id","pure","field"],Bindings ["id","old","pure"])
  , ("ImportedMethod","root",["old"],Bindings ["old"])
  , ("LocalShadow","root",["old"],Bindings [])
  , ("DuplicateFields","root",["field","old"],Bindings [])
  , ("ClassShadow","root",["old"],Bindings [])
  , ("ImportedRecord","root",["old"],Bindings ["old"])
  , ("LocalRecord","root",["old","field"],Bindings ["old"])
  , ("ImportedFieldShadow","root",["old"],Bindings ["old"])
  , ("ParseOnlyQuoteFlag","root",["old"],Bindings [])
  , ("QualifiedField","root",["versionBranch","old"],Bindings ["old"])
  , ("AliasNamespace","root",["versionBranch"],Bindings [])
  , ("AliasSynonyms","root",["first","second"],Bindings ["first","second"])
  , ("RenameError","root",["old"],RenameRefusal)
  , ("MissingImport","root",["old"],RenameRefusal)
  , ("ActualQuote","root",[],ParseRefusal)
  , ("DeclarationModule","",["old"],Bindings ["old"])
  , ("DeclarationQuote","",[],ParseRefusal)
  ]
