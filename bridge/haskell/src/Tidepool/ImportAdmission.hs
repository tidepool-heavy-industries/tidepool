{-# LANGUAGE OverloadedStrings #-}

-- | Request-scoped source import admission and its completed finite receipt.
-- Lower exact-scope validation consumes the existing scope and import facts;
-- only this upper owner pairs them with the actual compiled root.
module Tidepool.ImportAdmission
  ( ExactCompilation, compilationScope, compilationTransaction, compilationSourceImports
  , compilationExactImports, compilationSourceSelection, compilationSource
  , IssuedSourceImports, sourceImportsSnapshot, sourceImportsHomeRoles
  , compilationOriginalSourceImports
  , ExactCompilationPublication(..)
  , writeCheckedExactCompilation
  , writeCheckedExactCompilationWithOutputsAndPublication
  , writeRetainedExactCompilation, writeRetainedExactCompilationWithOutputsAndPublication
  ) where

import Codec.CBOR.Write (toStrictByteString)
import qualified Codec.CBOR.Encoding as E
import Control.Exception (IOException, try, throwIO)
import Control.Monad (forM_, unless)
import qualified Crypto.Hash.SHA256 as SHA
import qualified Data.ByteString as BS
import qualified Data.Text as T
import Data.Word (Word64)
import Numeric (showHex)
import System.Directory (createDirectory, createDirectoryIfMissing, makeAbsolute)
import System.FilePath (takeDirectory, (</>))
import System.IO.Error (isAlreadyExistsError)
import GHC.Driver.Env (HscEnv)
import Tidepool.DependencyEvidence
  ( DependencyEvidence, DependencySource(..), DependencyQualifier
  , dependencySources, revalidateDependencyEvidence, renderDependencyQualifier, renderDependencyEvidence )
import Tidepool.ExactScope
  ( ExactScope, scopeRequestSha256, scopeSemanticSha256
  , SourceSelectedOriginals, encodeSourceSelectedOriginals, revalidateSourceSelectedOriginals
  , CanonicalSourceImport, originalSourceImports
  , extendSourceSelectedOriginals
  , ExactScopeValidationReason(..), FreshOutputSeals, emptyFreshOutputSeals
  , revalidateExactScopesAtWithOutputs )
import Tidepool.ImportAdmission.Internal (ExactCompilation(..), CompletedFiniteImportAdmission(..), ImportAttempt(..), IssuedSourceImports(..))

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

compilationOriginalSourceImports
  :: ExactCompilation -> DependencyEvidence -> (String,String)
  -> Either String [CanonicalSourceImport]
compilationOriginalSourceImports = originalSourceImports . compilationExactImports

digest :: BS.ByteString -> String
digest = concatMap hex . BS.unpack . SHA.hash
  where hex value = case showHex value "" of [one] -> ['0',one]; two -> two

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
