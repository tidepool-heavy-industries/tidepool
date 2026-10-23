-- | Private representation shared by the actual pipeline issuer and the
-- public read-only admission facade. Importing this module is an issuer
-- obligation: constructors must never be re-exported by a caller adapter.
module Tidepool.ImportAdmission.Internal
  ( CapturedImportAdmission(..), ImportAdmissionContext(..)
  , ImportAttempt(..), ScopedImportAdmission(..)
  , ResolvedImportAdmission(..), ResolvedImportOccurrence(..)
  , PreparedImportAdmission(..), CompletedImportAdmission(..)
  , ImportCompletionContext(..)
  , ExactCompilation(..)
  ) where

import qualified Data.ByteString as BS
import Data.Word (Word64)
import GHC (Module, ModSummary)
import GHC.Fingerprint.Type (Fingerprint)
import GHC.Unit.Module (ModuleName)
import GHC.Types.PkgQual (PkgQual, RawPkgQual)
import Tidepool.ArtifactBytes (ArtifactBytes)
import Tidepool.CheckedCell (NativeParsedModule)
import Tidepool.ExactHydration (CapturedGeneratedImports, ExactIfaceArtifact, IssuedSourceImports)
import Tidepool.ExactScope (ExactScope, SourceSelectedOriginals)
import Tidepool.DependencyEvidence (DependencyQualifier)
import Tidepool.ImportSyntax (DriverImportSyntax, ImportOccurrenceId)
import Tidepool.TurnSource (ImportOrigin)

-- Ordinary source deliberately has no raw import census: CPP and other
-- admitted preprocessing select its imports in the actual driver buffer.
data CapturedImportAdmission
  = GeneratedImportCapture CapturedGeneratedImports
  | OrdinaryImportCapture FilePath ArtifactBytes Fingerprint

-- These are the issuing operation's already admitted prerequisites. World
-- carriage keeps only the actual selected lexical rows and existing identity
-- bytes, rather than retaining a world, HscEnv or available inventory.
data ImportAdmissionContext
  = OrdinaryImportContext
  | FiniteImportContext ExactScope
  | WorldImportContext BS.ByteString BS.ByteString Word64
      [(ExactIfaceArtifact,[(String,String)])] [(ModuleName,String)]

-- The timing request number is only one observation of the attempt. Full root,
-- captured source and incarnation remain independently bound throughout the
-- transitions; a matching number alone is never an admission proof.
data ImportAttempt = ImportAttempt Word64 (Maybe String) Module FilePath Fingerprint

data ScopedImportAdmission = ScopedImportAdmission
  CapturedImportAdmission ImportAdmissionContext ImportAttempt

data ResolvedImportOccurrence = ResolvedImportOccurrence
  ImportOccurrenceId ImportOrigin ModuleName RawPkgQual PkgQual Bool Module

data ResolvedImportAdmission = ResolvedImportAdmission
  ScopedImportAdmission DriverImportSyntax [ResolvedImportOccurrence]
  (Maybe SourceSelectedOriginals)

-- The only constructor sites are the closed actual frontend recipes. The
-- NativeParsedModule includes the same driver's import lineage and type uses.
data PreparedImportAdmission = PreparedImportAdmission
  ResolvedImportAdmission ModSummary NativeParsedModule

-- Successful compilation drops the parsed module and refinement buffer. The
-- completed owner retains only the exact decisions required by its receipt.
data CompletedImportAdmission = CompletedImportAdmission
  ImportAttempt ImportCompletionContext IssuedSourceImports
  [ResolvedImportOccurrence] (Maybe SourceSelectedOriginals)

data ImportCompletionContext
  = FiniteCompletionContext String String String
  | WorldCompletionContext BS.ByteString BS.ByteString Word64

-- No public record labels can attach a completed result to a different scope.
-- The actual pipeline issuer constructs this only from its own successful
-- frontend and the same finite scoped admission.
data ExactCompilation = ExactCompilation
  ExactScope Word64 IssuedSourceImports
  [((String,String,Bool),[(DependencyQualifier,String,Bool,String)])]
  (Maybe SourceSelectedOriginals)
  deriving (Eq, Show)
