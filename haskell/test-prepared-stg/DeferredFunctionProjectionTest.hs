{-# LANGUAGE OverloadedStrings #-}

module DeferredFunctionProjectionTest (verifyDeferredFunctionProjection) where

import Control.Monad (unless)
import Control.Monad.IO.Class (liftIO)
import Data.List (find)
import Data.Map.Strict qualified as Map
import Data.Text qualified as Text
import GHC.StgToCmm.Closure (importedIdLFInfo)
import GHC.StgToCmm.Types (LambdaFormInfo(LFReEntrant))
import GHC qualified
import GHC.Tc.Types (tcg_rdr_env)
import GHC.Types.Id (Id, idType)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Name.Reader (globalRdrEnvElts, greName)
import GHC.Types.TyThing (TyThing(AnId))
import GHC.Types.Var (varName)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import System.Directory (getCurrentDirectory)
import System.FilePath ((</>))
import System.Process (readProcess)

import Tidepool.ExecutionProjection
  ( ProjectionContext(..), preparedTargetReferences
  , projectPreparedTarget )
import Tidepool.ExecutionSchema
import Tidepool.GhcPipeline
  ( PipelineResult(prHscEnv), PipelineSelection(PreparedStg)
  , PreparedPipelineResult(..), runPipelineSelected )
import Tidepool.PreparedBuiltins (DeferredFunction(..), deferredFunction)
import Tidepool.PreparedRecovery (RecoveredClosure(..), recoverPreparedClosure)
import Tidepool.PreparedStg (PreparedModule)

data Expected = Expected
  { expectedTarget :: String
  , expectedModule :: String
  , expectedOccurrence :: String
  , expectedDeferred :: DeferredFunction
  }

verifyDeferredFunctionProjection :: IO ()
verifyDeferredFunctionProjection = do
  root <- getCurrentDirectory
  let source = root </> "test-prepared-stg" </> "DeferredFunctionProjection.hs"
  prepared <- runPipelineSelected PreparedStg
    source
    [root </> "test-prepared-stg"]
  mapM_ (verifyImported prepared) expectations
  collect <- loadCollectStackTrace source
  verifyLoadedId False (requiredExpected "collectStackTrace") collect
  verifyLookalikes prepared

expectations :: [Expected]
expectations =
  [ Expected "stackFramesBare" "GHC.Internal.ExecutionStack.Internal" "stackFrames"
      (DeferredFunction "ghc:stackFrames"
        (Signature [LiftedRefRep] (Returns [LiftedRefRep])))
  , Expected "getStackTraceBare" "GHC.Internal.ExecutionStack.Internal"
      "collectStackTrace1" (DeferredFunction "ghc:collectStackTrace"
        (Signature [VoidRep] (Returns [LiftedRefRep])))
  , Expected "ccsToStringsBare" "GHC.Internal.Stack.CCS" "$wgo"
      (DeferredFunction "ghc:ccsToStrings"
        (Signature [AddressRep, LiftedRefRep, VoidRep] (Returns [LiftedRefRep])))
  ]

verifyImported :: PreparedPipelineResult -> Expected -> IO ()
verifyImported prepared expected = do
  let context = fixtureContext (expectedTarget expected)
  closure <- recoverPreparedClosure (prHscEnv (pprPipelineResult prepared)) context
    (pprModules prepared)
  let references = preparedTargetReferences context (closureModules closure)
  binder <- case filter (matches expected) references of
    [value] -> pure value
    values -> ioError (userError ("expected one pinned interface Id for "
      <> expectedOccurrence expected <> ", got " <> show (length values)
      <> "; references: " <> show (map renderId references)))
  verifyLoadedId True expected binder
  program <- projectOrFail context (closureModules closure)
  assertCapability expected program
  unless (null [ failure | failure <- closureFailures closure
      , Text.pack (expectedOccurrence expected) `Text.isInfixOf` Text.pack (show failure) ])
    (ioError (userError ("catalogued Id remained a recovery residual: "
      <> expectedOccurrence expected)))
  pure ()

verifyLoadedId :: Bool -> Expected -> Id -> IO ()
verifyLoadedId requireEntry expected binder = do
  unless (matches expected binder
      && deferredFunction binder == Just (expectedDeferred expected))
    (ioError (userError ("deferred catalog mismatch for actual interface Id "
      <> renderId binder <> " :: " <> showSDocUnsafe (ppr (idType binder)))))
  if requireEntry
    then case importedIdLFInfo binder of
      LFReEntrant _ arity _ _ -> unless
        (arity == length (signatureArguments (deferredSignature (expectedDeferred expected))))
        (ioError (userError ("pinned LF arity mismatch for " <> renderId binder)))
      other -> ioError (userError ("pinned Id is not reentrant: " <> renderId binder
        <> " " <> showSDocUnsafe (ppr other)))
    else pure ()

loadCollectStackTrace :: FilePath -> IO Id
loadCollectStackTrace source = do
  libdir <- init <$> readProcess "ghc" ["--print-libdir"] ""
  GHC.runGhc (Just libdir) $ do
    flags <- GHC.getSessionDynFlags
    _ <- GHC.setSessionDynFlags flags
    target <- GHC.guessTarget source Nothing Nothing
    GHC.setTargets [target]
    _ <- GHC.load GHC.LoadAllTargets
    summary <- GHC.getModSummary (GHC.mkModuleName "DeferredFunctionProjection")
    parsed <- GHC.parseModule summary
    typed <- GHC.typecheckModule parsed
    let (tcEnv, _) = GHC.tm_internals_ typed
        names =
          [ name
          | gre <- globalRdrEnvElts (tcg_rdr_env tcEnv)
          , let name = greName gre
          , Just owner <- [nameModule_maybe name]
          , unitString (moduleUnit owner) == "ghc-internal"
          , moduleNameString (moduleName owner)
              == "GHC.Internal.ExecutionStack.Internal"
          , occNameString (nameOccName name) == "collectStackTrace"
          ]
    name <- case names of
      value : _ -> pure value
      [] -> liftIO (ioError (userError "loaded interface omitted collectStackTrace"))
    thing <- GHC.lookupName name
    case thing of
      Just (AnId binder) -> pure binder
      _ -> liftIO (ioError (userError "collectStackTrace did not resolve to an Id"))

verifyLookalikes :: PreparedPipelineResult -> IO ()
verifyLookalikes prepared = mapM_ verify
  ["stackFrames", "collectStackTrace", "collectStackTrace1"]
 where
  verify occurrence = do
    let context = fixtureContext occurrence
        matching = filter ((== occurrence) . occurrenceOf)
          (preparedTargetReferences context (pprModules prepared))
    unless (all ((== Nothing) . deferredFunction) matching)
      (ioError (userError ("source-module lookalike entered deferred catalog: " <> occurrence)))
    program <- projectOrFail context (pprModules prepared)
    unless (all (not . isDeferredCapability . operationIdentity) (programOperations program))
      (ioError (userError ("source-module lookalike emitted a deferred capability: " <> occurrence)))

projectOrFail :: ProjectionContext -> [PreparedModule] -> IO WireProgram
projectOrFail context modules = case projectPreparedTarget context modules of
  Left failure -> ioError (userError ("deferred function projection failed: " <> show failure))
  Right program -> pure program

assertCapability :: Expected -> WireProgram -> IO ()
assertCapability expected program = case
    [ signatureAt program signature
    | OperationDecl identity signature <- programOperations program
    , identity == CapabilityIdentity (deferredCapability (expectedDeferred expected))
    ] of
  [signature] | signature == deferredSignature (expectedDeferred expected) -> pure ()
  signatures -> ioError (userError ("wrong capability signature for "
    <> expectedOccurrence expected <> ": " <> show signatures))

signatureAt :: WireProgram -> SignatureId -> Signature
signatureAt program (SignatureId index) =
  programSignatures program !! fromIntegral index

fixtureContext :: String -> ProjectionContext
fixtureContext occurrence = ProjectionContext
  "ghc-9.12-prepared-stg" "ghc-9.12.2"
  (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty
  (SymbolIdentity "main" "DeferredFunctionProjection" "value"
    (Text.pack occurrence) Nothing) Nothing

matches :: Expected -> Id -> Bool
matches expected binder = case nameModule_maybe (varName binder) of
  Just owner -> unitString (moduleUnit owner) == "ghc-internal"
    && moduleNameString (moduleName owner) == expectedModule expected
    && occurrenceOf binder == expectedOccurrence expected
  Nothing -> False

occurrenceOf :: Id -> String
occurrenceOf = occNameString . nameOccName . varName

renderId :: Id -> String
renderId binder = case nameModule_maybe (varName binder) of
  Just owner -> unitString (moduleUnit owner) <> ":"
    <> moduleNameString (moduleName owner) <> ":" <> occurrenceOf binder
  Nothing -> occurrenceOf binder

symbolFor :: Id -> SymbolIdentity
symbolFor binder = case nameModule_maybe (varName binder) of
  Just owner -> SymbolIdentity (Text.pack (unitString (moduleUnit owner)))
    (Text.pack (moduleNameString (moduleName owner))) "value"
    (Text.pack (occurrenceOf binder)) Nothing
  Nothing -> error "deferred interface Id has no module"

requiredExpected :: String -> Expected
requiredExpected occurrence = case find ((== occurrence) . expectedOccurrence)
    (collectExpectation : expectations) of
  Just expected -> expected
  Nothing -> error ("missing deferred expectation " <> occurrence)

collectExpectation :: Expected
collectExpectation = Expected "collectStackTraceBare"
  "GHC.Internal.ExecutionStack.Internal" "collectStackTrace"
  (DeferredFunction "ghc:collectStackTrace"
    (Signature [VoidRep] (Returns [LiftedRefRep])))

groupItems :: Group a -> [a]
groupItems (NonRecursive item) = [item]
groupItems (Recursive items) = items

topSymbol :: TopBinding -> SymbolIdentity
topSymbol (TopBinding symbol _) = symbol

isDeferredCapability :: OperationIdentity -> Bool
isDeferredCapability CapabilityIdentity{} = True
isDeferredCapability _ = False
