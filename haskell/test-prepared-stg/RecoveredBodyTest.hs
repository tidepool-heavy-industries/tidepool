{-# LANGUAGE GADTs #-}

module Main (main) where

import Control.Monad (unless)
import Control.Monad.IO.Class (liftIO)
import Data.List (intercalate)
import Data.Text qualified as Text
import GHC
import GHC.Core (Bind(..))
import GHC.Driver.Session (updOptLevel)
import GHC.Driver.Main (hscTidy)
import GHC.Types.Id (idName)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Var (varName)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import System.Directory (getCurrentDirectory)
import System.FilePath ((</>))
import System.Exit (ExitCode(..))
import System.Process (proc, readCreateProcessWithExitCode)
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), preparedTargetReferences, preparedTopIdentities
  , projectPreparedTarget )
import Tidepool.ExecutionSchema
  ( Architecture(..), Endianness(..), Group(..), SymbolIdentity(..)
  , TargetDescriptor(..), TopBinding(..), WireProgram(..) )
import Tidepool.FatIface (newFatIfaceCache)
import Tidepool.PreparedStg
  ( PreparedModule(..), RecoveredModuleFailure(..), prepareModule, prepareRecoveredBodies
  , unelaboratedModule )
import Tidepool.Resolve
  ( BodyOrigin(..), ExactBodyLookup(..), recoverExactBody )

assert :: Bool -> String -> IO ()
assert ok message = unless ok (ioError (userError message))

main :: IO ()
main = do
  root <- getCurrentDirectory
  let source = root </> "test-prepared-stg" </> "RecoveredBody.hs"
  libdir <- trim <$> readProcessGhc ["--print-libdir"]
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags (updOptLevel 0 flags)
      { importPaths = root : importPaths flags
      , backend = noBackend
      , ghcLink = NoLink
      }
    target <- guessTarget source Nothing Nothing
    setTargets [target]
    _ <- load LoadAllTargets
    summary <- getModSummary (mkModuleName "RecoveredBody")
    parsed <- parseModule summary
    typed <- typecheckModule parsed
    desugared <- desugarModule typed
    hsc <- getSession
    (callerGuts, _) <- liftIO $ hscTidy hsc (coreModule desugared)
    caller <- liftIO $ prepareModule hsc summary (unelaboratedModule callerGuts)
    let entry = callerEntry caller
        context = ProjectionContext
          { projectionProfile = Text.pack "w5-b2-recovered-body"
          , projectionToolchain = Text.pack "ghc-9.12.2"
          , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
              (Text.pack "sysv64") []
          , projectionRetainedGenerations = mempty
          , projectionEntry = entry
          }
        references = preparedTargetReferences context [caller]
    liftIO $ assert (any isFst references)
      ("-O0 caller did not retain a real package fst reference: "
        ++ intercalate ", " (map (showSDocUnsafe . ppr . idName) references))
    let fstIds = filter isFst references
    cache <- liftIO newFatIfaceCache
    recovered <- liftIO $ recoverFirst hsc cache fstIds
    case recovered of
      (fstId, ExactBody owner body origin) -> do
        liftIO $ assert (origin == InterfaceUnfolding || origin == FatInterfaceGroup)
          "exact recovery returned an unknown body origin"
        preparedResult <- liftIO $ prepareRecoveredBodies hsc owner (bindList body)
        recoveredModule <- case preparedResult of
          Left failure -> liftIO $ ioError (userError
            ("defining-context preparation failed: " ++ show failure))
          Right prepared -> pure prepared
        liftIO $ do
          assert (pmModule recoveredModule == owner)
            "recovered body was prepared under the caller module"
          assert (moduleNameString (moduleName owner) == recoveredModuleName fstId)
            "recovered body module identity did not come from the Id"
          case projectPreparedTarget context [caller, recoveredModule] of
            Left failure -> ioError (userError
              ("caller + exact body projection failed: " ++ show failure))
            Right program -> assert (hasRecoveredTop owner fstId program)
              "projection omitted the recovered defining top"
      (fstId, other) -> liftIO $ ioError (userError
        ("real package fst had no exact body (actual Id "
          ++ showSDocUnsafe (ppr (idName fstId)) ++ "): " ++ showLookup other))
  assertSemigroupSubset root libdir
  where
    callerEntry prepared = case
      [ identity
      | identity <- either (error . show) id (preparedTopIdentities [prepared])
      , symbolOccurrence identity == Text.pack "caller"
      ] of
      [identity] -> identity
      found -> error ("expected one caller entry, got " ++ show found)

    isFst identifier = occNameString (nameOccName (varName identifier)) == "fst"

    bindList (NonRec binder body) = [NonRec binder body]
    bindList (Rec pairs) = [Rec pairs]

    recoverFirst _ _ [] = error "recoverFirst called with no fst Id"
    recoverFirst hsc cache (identifier : rest) = do
      result <- recoverExactBody hsc cache identifier
      case result of
        exact@(ExactBody _ _ _) -> pure (identifier, exact)
        _ | null rest -> pure (identifier, result)
          | otherwise -> recoverFirst hsc cache rest

    recoveredModuleName identifier = case nameModule_maybe (varName identifier) of
      Just owner -> moduleNameString (moduleName owner)
      Nothing -> error "fst Id unexpectedly had no defining module"

    hasRecoveredTop owner identifier program = any matches
      [ symbol
      | group <- programBindings program
      , top <- groupItems group
      , symbol <- [topSymbol top]
      ]
      where
        wantedOccurrence = Text.pack (occNameString (nameOccName (varName identifier)))
        matches symbol = symbolModule symbol == Text.pack (moduleNameString (moduleName owner))
          && symbolOccurrence symbol == wantedOccurrence
        topSymbol (TopBinding symbol _) = symbol
        groupItems (NonRecursive top) = [top]
        groupItems (Recursive tops) = tops

    showLookup (ExactBody owner _ origin) = "exact body in " ++ renderModule owner ++ " via " ++ show origin
    showLookup (MissingExactBody name reason) = "missing " ++ renderName name ++ ": " ++ show reason
    showLookup (BodyInterfaceFailure owner reason) = "interface failure in " ++ renderModule owner ++ ": " ++ reason
    showLookup (UnsupportedBodyCapability name) = "unsupported body " ++ renderName name

    renderModule = showSDocUnsafe . ppr
    renderName = showSDocUnsafe . ppr

    trim = reverse . dropWhile (== '\n') . reverse

    readProcessGhc args = do
      (code, out, err) <- readCreateProcessWithExitCode (proc "ghc" args) ""
      case code of
        ExitSuccess -> pure out
        _ -> ioError (userError ("ghc failed: " ++ err))

assertSemigroupSubset :: FilePath -> String -> IO ()
assertSemigroupSubset root libdir = runGhc (Just libdir) $ do
  flags <- getSessionDynFlags
  _ <- setSessionDynFlags (updOptLevel 0 flags)
    { importPaths = [root </> "test-prepared-stg", root </> "lib"] ++ importPaths flags
    , backend = noBackend
    , ghcLink = NoLink
    }
  target <- guessTarget (root </> "test-prepared-stg" </> "RecoveredBody.hs") Nothing Nothing
  setTargets [target]
  _ <- load LoadAllTargets
  summary <- getModSummary (mkModuleName "RecoveredBody")
  parsed <- parseModule summary
  typed <- typecheckModule parsed
  desugared <- desugarModule typed
  hsc <- getSession
  (guts, _) <- liftIO $ hscTidy hsc (coreModule desugared)
  prepared <- liftIO $ prepareModule hsc summary (unelaboratedModule guts)
  let context = ProjectionContext
        { projectionProfile = Text.pack "w5-b2-recovered-same-owner"
        , projectionToolchain = Text.pack "ghc-9.12.2"
        , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
            (Text.pack "sysv64") []
        , projectionRetainedGenerations = mempty
        , projectionEntry = SymbolIdentity
            (Text.pack "main") (Text.pack "RecoveredBody") (Text.pack "value")
            (Text.pack "foldableCaller") Nothing
        }
      references = preparedTargetReferences context [prepared]
      semigroupReferences = filter isSemigroupOwner references
      monoidProductReferences = filter isMonoidProduct semigroupReferences
  semigroupId <- case monoidProductReferences of
    [value] -> pure value
    found -> liftIO $ ioError (userError
      ("expected one defining semigroup reference, got " ++ show (length found)
        ++ ": " ++ intercalate ", " (map renderId found)))
  cache <- liftIO newFatIfaceCache
  lookupResult <- liftIO $ recoverExactBody hsc cache semigroupId
  (owner, body) <- case lookupResult of
    ExactBody owner group _ -> pure (owner, group)
    other -> liftIO $ ioError (userError
      ("semigroup reference was not recovered exactly: " ++ showLookup' other))
  recovered <- liftIO $ prepareRecoveredBodies hsc owner (bindList body)
  recoveredModule <- case recovered of
    Right value -> pure value
    Left failure -> liftIO $ ioError (userError
      ("same-owner recovered subset did not prepare: " ++ show failure))
  let recoveredReferences = preparedTargetReferences context [prepared, recoveredModule]
      productOneReferences = filter isMonoidProductOne recoveredReferences
  productOneId <- case productOneReferences of
    [value] -> pure value
    found -> liftIO $ ioError (userError
      ("expected one recovered $fMonoidProduct1 reference, got "
        ++ show (length found) ++ ": " ++ intercalate ", " (map renderId found)))
  productOneLookup <- liftIO $ recoverExactBody hsc cache productOneId
  (productOneOwner, productOneBody) <- case productOneLookup of
    ExactBody owner' group _ -> pure (owner', group)
    other -> liftIO $ ioError (userError
      ("$fMonoidProduct1 was not recovered exactly: " ++ showLookup' other))
  liftIO $ assert (productOneOwner == owner)
    "$fMonoidProduct1 defining owner changed during recovery"
  productOnePrepared <- liftIO $ prepareRecoveredBodies hsc productOneOwner
    (bindList productOneBody)
  productOneModule <- case productOnePrepared of
    Right value -> pure value
    Left failure -> liftIO $ ioError (userError
      ("$fMonoidProduct1 recovered subset did not prepare: " ++ show failure))
  let allRecovered = [prepared, recoveredModule, productOneModule]
      allReferences = preparedTargetReferences context allRecovered
  liftIO $ assert (any isStimes allReferences)
    ("stimesMonoid1 dependency disappeared from references: "
      ++ intercalate ", " (map renderId allReferences))
  sourceHome <- liftIO $ prepareRecoveredBodies hsc (pmModule prepared) []
  case sourceHome of
    Left RecoveredModuleFinderFailure{} -> pure ()
    Left failure -> liftIO $ ioError (userError
      ("source-home recovery reported wrong failure: " ++ show failure))
    Right _ -> liftIO $ ioError (userError
      "source-home module was incorrectly admitted as recovered subset")
  where
    isSemigroupOwner identifier = case nameModule_maybe (varName identifier) of
      Just owner -> moduleNameString (moduleName owner)
        == "GHC.Internal.Data.Semigroup.Internal"
      Nothing -> False
    isStimes identifier = occNameString (nameOccName (varName identifier))
      == "stimesMonoid1"
    isMonoidProduct identifier = occNameString (nameOccName (varName identifier))
      == "$fMonoidProduct"
    isMonoidProductOne identifier = occNameString (nameOccName (varName identifier))
      == "$fMonoidProduct1"
    bindList (NonRec binder body) = [NonRec binder body]
    bindList (Rec pairs) = [Rec pairs]
    renderId identifier = showSDocUnsafe (ppr (idName identifier))
    showLookup' (ExactBody owner _ origin) = "exact body in " ++ renderModule' owner ++ " via " ++ show origin
    showLookup' (MissingExactBody name reason) = "missing " ++ renderName' name ++ ": " ++ show reason
    showLookup' (BodyInterfaceFailure owner reason) = "interface failure in " ++ renderModule' owner ++ ": " ++ reason
    showLookup' (UnsupportedBodyCapability name) = "unsupported body " ++ renderName' name
    renderModule' = showSDocUnsafe . ppr
    renderName' = showSDocUnsafe . ppr
