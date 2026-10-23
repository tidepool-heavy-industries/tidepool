-- | Private representation shared by the actual pipeline issuer and the
-- public read-only admission facade. Importing this module is an issuer
-- obligation: constructors must never be re-exported by a caller adapter.
module Tidepool.ImportAdmission.Internal
  ( CapturedImportAdmission(..), ImportAdmissionContext(..)
  , ImportAttempt(..), ScopedImportAdmission(..)
  , MatchedImportDriver(..), ImportDriverOccurrence(..)
  , ResolvedImportAdmission(..), ResolvedImportOccurrence(..)
  , PreparedImportAdmission(..), CompletedImportAdmission(..)
  , CompletedFiniteImportAdmission(..), CompletedWorldImportAdmission(..), WorldImportCompletionIdentity(..)
  , IssuedSourceImports(..), ExactCompilation(..)
  ) where

import qualified Data.ByteString as BS
import Data.Word (Word64)
import GHC (Module, ModSummary)
import GHC.Fingerprint.Type (Fingerprint)
import GHC.Unit.Module (ModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import GHC.Types.PkgQual (PkgQual, RawPkgQual)
import Tidepool.ArtifactBytes (ArtifactBytes)
import Tidepool.CheckedCell (NativeParsedModule)
import Tidepool.ExactHydration (CapturedGeneratedImports, ExactIfaceArtifact, CheckedValueImportAuthority)
import Tidepool.ExactScope (ExactScope, SourceSelectedOriginals)
import Tidepool.DependencyEvidence (DependencyQualifier)
import Tidepool.ImportSyntax (DriverImportSyntax, ImportOccurrenceId)
import Tidepool.Binders (ImportIntent)
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
  deriving (Eq)

instance Show ImportAttempt where
  show (ImportAttempt request incarnation root path fingerprint) =
    "ImportAttempt " ++ show
      (request, incarnation, (unitString (moduleUnit root), moduleNameString (moduleName root)), path, fingerprint)

data ScopedImportAdmission = ScopedImportAdmission
  CapturedImportAdmission ImportAdmissionContext ImportAttempt

-- Syntax correspondence is data, not a selected-owner proof. Exact source-
-- less interfaces are installed selectively immediately before their actual
-- frontend, so no Finder observation is allowed to stand in for that issuer.
data ImportDriverOccurrence = ImportDriverOccurrence
  ImportOccurrenceId ImportOrigin ModuleName RawPkgQual PkgQual Bool

data MatchedImportDriver = MatchedImportDriver
  ScopedImportAdmission DriverImportSyntax [ImportDriverOccurrence]
  (Maybe SourceSelectedOriginals)

data ResolvedImportOccurrence = ResolvedImportOccurrence
  ImportOccurrenceId ImportOrigin ModuleName RawPkgQual PkgQual Bool Module

data ResolvedImportAdmission = ResolvedImportAdmission
  MatchedImportDriver [ResolvedImportOccurrence]

-- The only constructor sites are the closed actual frontend recipes. The
-- NativeParsedModule includes the same driver's import lineage and type uses.
data PreparedImportAdmission = PreparedImportAdmission
  ResolvedImportAdmission ModSummary NativeParsedModule CheckedValueImportAuthority

-- Successful compilation drops the parsed module and refinement buffer. The
-- completed owner retains only the exact decisions required by its receipt.
data CompletedImportAdmission
  = CompletedFiniteImports CompletedFiniteImportAdmission
  | CompletedWorldImports CompletedWorldImportAdmission
  | CompletedOrdinaryImports ImportAttempt

data CompletedFiniteImportAdmission = CompletedFiniteImportAdmission
  ExactScope ImportAttempt IssuedSourceImports [(ImportIntent,DependencyQualifier,(String,String))]
  (Maybe SourceSelectedOriginals)
  deriving (Eq, Show)

data CompletedWorldImportAdmission = CompletedWorldImportAdmission
  WorldImportCompletionIdentity ImportAttempt IssuedSourceImports

data WorldImportCompletionIdentity = WorldImportCompletionIdentity
  { completedWorldInputsIdentity :: BS.ByteString
  , completedWorldCompilerIdentity :: BS.ByteString
  , completedWorldBindingScope :: Word64
  }

-- This compact census is issued only after successful compilation. Its rows
-- retain the selected HOME owner for every terminal role; package syntax stays
-- in the private complete occurrence census.
data IssuedSourceImports = IssuedSourceImports
  FilePath BS.ByteString BS.ByteString (String,String)
  [((String,String,Bool),Int)] [(String,String,Bool,String,Int)]
  deriving (Eq)

instance Show IssuedSourceImports where
  show (IssuedSourceImports path _ _ root occurrences roles) =
    "IssuedSourceImports " ++ show (path,root,length occurrences,length roles)

-- A finite receipt cannot be paired with a World or ordinary completion, nor
-- can a caller attach a different ExactScope to a successful finite result.
data ExactCompilation = ExactCompilation CompletedFiniteImportAdmission
  [((String,String,Bool),[(DependencyQualifier,String,Bool,String)])]
  deriving (Eq, Show)
