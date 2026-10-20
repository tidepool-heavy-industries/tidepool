module DemandScopeTest (demandScopeChecks) where

import Control.Exception (bracket)
import Control.Monad (forM_, unless)
import Control.Monad.IO.Class (liftIO)
import qualified Data.Map.Strict as Map
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
        source <- liftIO (readFile
          ("test-cell-splitter/fixtures/demand-scope" </> name ++ ".hs"))
        actual <- liftIO (sourceModuleFlags initial source)
          >>= either (liftIO . fail . show) pure
        _ <- setSessionDynFlags actual {backend=noBackend,ghcLink=NoLink}
        environment <- getSession
        liftIO $ do
          let path = directory </> name ++ ".hs"
              bindings = Map.fromList (zip aliases [0 :: Int ..])
          writeFile path source
          plan <- either (fail . show) pure
            (typedSegmentPlan (replicate 64 '0') root
              [TypedItemPlan 0 "item" 1 (LetItem "marker" [])])
          recipe <- generatedCheckingTemplateRecipe actual (CheckedTemplateImports [] [])
            source source path name >>= either fail pure
          captured <- captureGeneratedScaffoldSource recipe path >>= either fail pure
          scope <- newSegmentDemandScope environment bindings
          case prepareSegmentDemand plan actual captured of
            Left _ | expected == ParseRefusal -> pure ()
            Left failure -> fail (name ++ ": parsing refused " ++ show failure)
            Right _ | expected == ParseRefusal -> fail (name ++ ": quote passed the parser gate")
            Right parsed -> do
              demanded <- typedSegmentInputOccurrences environment scope parsed
              case (expected,demanded) of
                (Bindings names,Right selected) | map fst selected == names -> pure ()
                (RenameRefusal,Left _) -> pure ()
                _ -> fail (name ++ ": wrong demand or refusal phase")
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
  , ("ActualQuote","root",[],ParseRefusal)
  ]
