{-# LANGUAGE OverloadedStrings #-}

-- | Request-scoped source import admission and its completed finite receipt.
-- Lower exact-scope validation consumes the existing scope and import facts;
-- only this upper owner pairs them with the actual compiled root.
module Tidepool.ImportAdmission
  ( CheckedItemImportRendering, checkedItemImportRendering
  , checkedItemOriginalRendering, checkedItemValueRendering, checkedItemImportNamespaces
  , appendCompletedProgramImports
  , ExactCompilation, compilationScope, compilationTransaction, compilationSourceImports
  , compilationExactImports, compilationSourceSelection, compilationSource
  , IssuedSourceImports, sourceImportsSnapshot, sourceImportsHomeRoles
  , captureFinalizedSourceOriginals, compilationOriginalSourceImports
  , ExactCompilationPublication(..)
  , writeExactCompilation, writeCheckedExactCompilation
  , writeCheckedExactCompilationWithOutputsAndPublication
  , writeRetainedExactCompilation, writeRetainedExactCompilationWithOutputsAndPublication
  ) where

import Codec.CBOR.Write (toStrictByteString)
import qualified Codec.CBOR.Encoding as E
import Control.Exception (IOException, try, throwIO)
import Control.Monad (forM_, unless)
import Data.List (intercalate)
import qualified Crypto.Hash.SHA256 as SHA
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import qualified Data.Text as T
import Data.Word (Word64)
import Numeric (showHex)
import System.Directory (createDirectory, createDirectoryIfMissing, makeAbsolute)
import System.FilePath (takeDirectory, (</>))
import System.IO.Error (isAlreadyExistsError)
import GHC.Driver.Env (HscEnv)
import GHC.Unit.Module (ModuleName, mkModuleName)
import Tidepool.DependencyEvidence
  ( DependencyEvidence, DependencySource(..), DependencyQualifier
  , dependencySources, revalidateDependencyEvidence, renderDependencyQualifier, renderDependencyEvidence )
import Tidepool.ExactScope
  ( ExactScope, scopeCheckedCell, scopeCheckedItem, scopeLexical, scopeInterfaceEvidence, scopeInterfaces, scopeValueInterfaces
  , CheckedItemAdmission(..), ExactInterfaceEvidence(..)
  , scopeRequestSha256, scopeSemanticSha256
  , SourceSelectedOriginals, encodeSourceSelectedOriginals, revalidateSourceSelectedOriginals
  , CanonicalInterfaceProof, CanonicalSourceImport, originalSourceImports
  , captureCheckedSourceOriginals, extendSourceSelectedOriginals
  , ExactScopeValidationReason(..), FreshOutputSeals, emptyFreshOutputSeals
  , revalidateExactScopesAtWithOutputs )
import Tidepool.ModuleCandidates (ModuleCandidate)
import Tidepool.FinalizedModuleArtifacts (FinalizedModuleArtifacts, localFinalizedInterface)
import Tidepool.LocalNativeDeclaration (localNativeOwner, localNativeProof)
import Tidepool.CheckedPrefixImports (CompletedValueImport(..))
import Tidepool.Binders (CellSourcePlan(..), SourcePrologue(..))
import Tidepool.ExactHydration (ExactIfaceArtifact(..))
import Tidepool.ImportAdmission.Internal (ExactCompilation(..), CompletedFiniteImportAdmission(..), ImportAttempt(..), IssuedSourceImports(..))
import Tidepool.ImportSyntax.Internal
  ( CellSourceSpan(..), LocatedImport(..), ImportIntent(..)
  , ProtectedImports(..), protectedImportsRendering )
import Tidepool.TurnSource (ImportRendering, renderImportBinder, appendImportRendering, literalImportRendering)

-- Render only the imports already attached to this exact checked item. The
-- returned syntax carries no new lexical or native grant; subsequent admission
-- still authenticates the exact selected owners and imported binder contracts.
data CheckedItemImportRendering = CheckedItemImportRendering
  (Maybe ImportRendering) (Maybe ImportRendering) [ModuleName]

checkedItemImportRendering :: ExactScope -> CheckedItemAdmission
  -> Either String CheckedItemImportRendering
checkedItemImportRendering scope item = do
  unless (scopeCheckedItem scope == Just item)
    (Left "checked import rendering belongs to another item admission")
  let original = fmap (\((_,owner),_) -> protectedImportsRendering
        (ProtectedImports ("import " ++ owner ++ "\n"))) (itemPlannedDeclaration item)
      values = case itemValueImports item of
        [] -> Nothing
        imports -> Just (foldl appendImportRendering (literalImportRendering "")
          [protectedImportsRendering (ProtectedImports
            ("import " ++ owner ++ " (" ++ intercalate ", " (map renderImportBinder names) ++ ")\n"))
          | (owner,names) <- imports])
      namespaces = maybe [] (\((_,owner),_) -> [mkModuleName owner]) (itemPlannedDeclaration item)
        ++ map (mkModuleName . fst) (itemValueImports item)
  pure (CheckedItemImportRendering original values namespaces)

checkedItemOriginalRendering :: CheckedItemImportRendering -> Maybe ImportRendering
checkedItemOriginalRendering (CheckedItemImportRendering original _ _) = original

checkedItemValueRendering :: CheckedItemImportRendering -> Maybe ImportRendering
checkedItemValueRendering (CheckedItemImportRendering _ values _) = values

checkedItemImportNamespaces :: CheckedItemImportRendering -> [ModuleName]
checkedItemImportNamespaces (CheckedItemImportRendering _ _ namespaces) = namespaces

-- Same-cell declaration imports derive from retained local native admission,
-- not merely from payload custody. Values retain their independent Protected
-- route and must match the exact checked-value interface already in the scope.
appendCompletedProgramImports :: ExactScope -> [CompletedValueImport]
  -> [(String,String)] -> CellSourcePlan -> Either String CellSourcePlan
appendCompletedProgramImports scope values originals plan = do
  unless (maybe False (const True) (scopeCheckedCell scope))
    (Left "program prefix imports lack their checked cell admission")
  forM_ originals $ \owner -> do
    unless (owner `elem` map fst (scopeLexical scope))
      (Left "program declaration import lacks its admitted lexical owner")
    case Map.lookup owner (scopeInterfaceEvidence scope) of
      Just (LocalNativeDeclarationEvidence native)
        | localNativeOwner native == owner
        , [localFinalizedInterface (localNativeProof native)] ==
            [row | row@(artifact,_,_) <- scopeInterfaces scope
              , (exactUnit artifact,exactModule artifact) == owner] -> pure ()
      _ -> Left "program declaration import lacks its retained local native admission"
  forM_ values $ \value -> unless
    (any (\artifact -> exactUnit artifact == completedValueUnit value
      && exactModule artifact == completedValueModule value
      && exactPath artifact == completedValueIfacePath value
      && exactSha256 artifact == completedValueIfaceSha256 value) (scopeValueInterfaces scope))
    (Left "program value import differs from its checked interface")
  let prologue = cellPlanPrologue plan
      imports = [LocatedImport (CellSourceSpan 1 1 1 1) ("import " ++ owner)
          InheritedLexicalImport [mkModuleName owner] | (_,owner) <- originals]
        ++ [LocatedImport (CellSourceSpan 1 1 1 1)
          ("import " ++ completedValueModule value ++ " ("
            ++ intercalate ", " (map (renderImportBinder . fst) (completedValueBinders value)) ++ ")")
          ProtectedCompilerImport [mkModuleName (completedValueModule value)] | value <- values]
  pure plan {cellPlanPrologue = prologue {prologueImports = prologueImports prologue ++ imports}}


compilationScope :: ExactCompilation -> ExactScope
compilationScope (ExactCompilation (CompletedFiniteImportAdmission scope _ _ _ _) _) = scope

compilationTransaction :: ExactCompilation -> Word64
compilationTransaction (ExactCompilation (CompletedFiniteImportAdmission _ (ImportAttempt transaction _ _ _ _) _ _ _) _) = transaction

compilationSourceImports :: ExactCompilation -> IssuedSourceImports
compilationSourceImports (ExactCompilation (CompletedFiniteImportAdmission _ _ imports _ _) _) = imports

compilationExactImports :: ExactCompilation
  -> [((String,String,Bool),[(DependencyQualifier,String,Bool,String)])]
compilationExactImports (ExactCompilation _ imports) = imports

compilationSourceSelection :: ExactCompilation -> Maybe SourceSelectedOriginals
compilationSourceSelection (ExactCompilation (CompletedFiniteImportAdmission _ _ _ _ selected) _) = selected

sourceImportsSnapshot :: IssuedSourceImports -> (FilePath,BS.ByteString)
sourceImportsSnapshot (IssuedSourceImports path _ digest _ _ _) = (path,digest)

sourceImportsHomeRoles :: IssuedSourceImports -> [(String,String,Bool,String,Int)]
sourceImportsHomeRoles (IssuedSourceImports _ _ _ _ _ roles) = roles

compilationSource :: ExactCompilation -> FilePath
compilationSource = fst . sourceImportsSnapshot . compilationSourceImports

captureFinalizedSourceOriginals
  :: ExactCompilation -> [ModuleCandidate] -> (String,String) -> FinalizedModuleArtifacts
  -> DependencyEvidence -> IO (Map.Map (String,String) CanonicalInterfaceProof)
captureFinalizedSourceOriginals compilation accepted target finalized evidence
  | Nothing <- scopeCheckedCell (compilationScope compilation) = pure Map.empty
  | otherwise = captureCheckedSourceOriginals (compilationScope compilation)
      (compilationExactImports compilation) accepted target finalized evidence

compilationOriginalSourceImports
  :: ExactCompilation -> DependencyEvidence -> (String,String)
  -> Either String [CanonicalSourceImport]
compilationOriginalSourceImports = originalSourceImports . compilationExactImports

digest :: BS.ByteString -> String
digest = concatMap hex . BS.unpack . SHA.hash
  where hex value = case showHex value "" of [one] -> ['0',one]; two -> two

-- Every successful compile owns a distinct immutable source snapshot. Check,
-- fold and inspection requests can consume several generated modules, so a
-- later successful transaction must not replace an earlier witness.
writeExactCompilation
  :: ExactCompilation -> DependencyEvidence -> IO ()
writeExactCompilation compilation evidence =
  captureExactCompilationReceipt compilation evidence >>= publishExactCompilationReceipt

-- Publication is data, so no compiler work or caller callback can intervene
-- between the terminal observations and the receipt. Certified bytes are
-- supplied by their product writer; this operation grants no artifact authority.
data ExactCompilationPublication
  = ReceiptOnly
  | PublishCertificate FilePath BS.ByteString

-- Metadata has completed all compiler work at this boundary. Capture and
-- validate every consumed source before the terminal current scope/package
-- proof; publishing the receipt afterwards consumes only captured bytes.
writeCheckedExactCompilation
  :: HscEnv -> ExactCompilation -> DependencyEvidence -> IO ()
writeCheckedExactCompilation env = writeCheckedExactCompilationWithScopes
  CheckedReceiptPublication env [] emptyFreshOutputSeals ReceiptOnly

writeCheckedExactCompilationWithOutputsAndPublication
  :: HscEnv -> ExactCompilation -> DependencyEvidence -> FreshOutputSeals -> ExactCompilationPublication -> IO ()
writeCheckedExactCompilationWithOutputsAndPublication env compilation evidence outputs publish =
  writeCheckedExactCompilationWithScopes CheckedReceiptPublication env [] outputs publish compilation evidence

-- Retention and the checked receipt share this final publication boundary.
-- Capture source evidence first, then check selected scopes and fresh outputs
-- in one proof. No observation or validation token reaches the publisher.
writeRetainedExactCompilation
  :: HscEnv -> ExactScope -> ExactCompilation -> DependencyEvidence -> IO ()
writeRetainedExactCompilation env retained = writeCheckedExactCompilationWithScopes
  RetainedReceiptPublication env [retained] emptyFreshOutputSeals ReceiptOnly

writeRetainedExactCompilationWithOutputsAndPublication
  :: HscEnv -> ExactScope -> ExactCompilation -> DependencyEvidence
  -> FreshOutputSeals -> ExactCompilationPublication -> IO ()
writeRetainedExactCompilationWithOutputsAndPublication env retained compilation evidence outputs publish =
  writeCheckedExactCompilationWithScopes RetainedReceiptPublication env [retained] outputs publish compilation evidence

writeCheckedExactCompilationWithScopes
  :: ExactScopeValidationReason -> HscEnv -> [ExactScope] -> FreshOutputSeals
  -> ExactCompilationPublication -> ExactCompilation -> DependencyEvidence -> IO ()
writeCheckedExactCompilationWithScopes reason env retained outputs publishCertificate compilation evidence = do
  selected <- either fail pure (extendSourceSelectedOriginals
    (compilationSourceSelection compilation) (compilationScope compilation))
  receipt <- captureExactCompilationReceipt compilation evidence
  revalidateExactScopesAtWithOutputs reason env (retained ++ [selected]) outputs >>= either fail pure
  case publishCertificate of
    ReceiptOnly -> pure ()
    PublishCertificate path bytes -> BS.writeFile path bytes
  publishExactCompilationReceipt receipt

-- Kept private so a receipt cannot be constructed from unobserved inputs.
data ExactCompilationReceipt = ExactCompilationReceipt
  FilePath Word64 BS.ByteString (FilePath -> BS.ByteString)

captureExactCompilationReceipt
  :: ExactCompilation -> DependencyEvidence -> IO ExactCompilationReceipt
captureExactCompilationReceipt compilation evidence = do
  let scope = compilationScope compilation
      transaction = compilationTransaction compilation
      (source,sourceSha) = sourceImportsSnapshot (compilationSourceImports compilation)
      imports = compilationExactImports compilation
  path <- makeAbsolute source
  bytes <- BS.readFile path
  unless (SHA.hash bytes == sourceSha)
    (fail "exact compile source differs from its issued import origins")
  unless (any (\item -> dependencySourcePath item == path
      && dependencySourceSha256 item == digest bytes) (dependencySources evidence))
    (fail "exact compile source differs from consumed source")
  unchanged <- revalidateDependencyEvidence evidence
  unless unchanged (fail "exact compile consumed source changed before receipt")
  forM_ (compilationSourceSelection compilation) $ \selected -> do
    unchangedSelection <- revalidateSourceSelectedOriginals selected
    unless unchangedSelection
      (fail "current original source selection changed before receipt")
  let parent = takeDirectory path </> ".exact-compilations"
      encodeArray values = E.encodeListLen (fromIntegral (length values)) <> mconcat values
      text = E.encodeString . T.pack
      importRow (qualifier, name, boot, unit) = encodeArray
        [text (renderDependencyQualifier qualifier), text name, E.encodeBool boot, text unit]
      moduleRow ((unit, name, boot), edges) = encodeArray
        [text unit, text name, E.encodeBool boot, encodeArray (map importRow edges)]
      receipt snapshot = toStrictByteString $ encodeArray
        [text "TPEXACTCOMPILE", text "4", text (scopeRequestSha256 scope)
        , text (scopeSemanticSha256 scope), text path, text (digest bytes)
        , text snapshot, text (renderDependencyEvidence evidence)
        , encodeArray (map moduleRow imports)
        , encodeSourceSelectedOriginals (compilationSourceSelection compilation)
        , encodeArray [encodeArray [text qualifier,text name,E.encodeBool boot,text unit,E.encodeInt role]
            | (qualifier,name,boot,unit,role) <- sourceImportsHomeRoles (compilationSourceImports compilation)]]
  pure (ExactCompilationReceipt parent transaction bytes receipt)

publishExactCompilationReceipt :: ExactCompilationReceipt -> IO ()
publishExactCompilationReceipt (ExactCompilationReceipt parent transaction bytes receipt) = do
  createDirectoryIfMissing True parent
  directory <- reserveCompilationDirectory parent transaction
  let snapshot = directory </> "source.hs"
  BS.writeFile snapshot bytes
  BS.writeFile (directory </> "receipt.cbor") (receipt snapshot)

-- One worker request can check, refine and compile several sources. The
-- request identity correlates diagnostics; it cannot identify one immutable
-- compilation snapshot. Atomic directory creation also keeps concurrent
-- writers from replacing an earlier successful receipt.
reserveCompilationDirectory :: FilePath -> Word64 -> IO FilePath
reserveCompilationDirectory parent transaction = attempt (0 :: Int)
  where
    attempt ordinal
      | ordinal >= 4096 = fail "excessive exact compilations in one request"
      | otherwise = do
          let path = parent </> (show transaction ++ "-" ++ show ordinal)
          reserved <- try (createDirectory path) :: IO (Either IOException ())
          case reserved of
            Right () -> pure path
            Left failure | isAlreadyExistsError failure -> attempt (ordinal + 1)
            Left failure -> throwIO failure
