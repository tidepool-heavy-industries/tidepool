-- | Readonly syntax evidence for a captured root and its actual driver parse.
-- This module issues no source, interface-loading, or native permission.
module Tidepool.ImportSyntax
  ( ImportDescriptor, describeImport
  , ImportOccurrenceId, ImportOccurrence
  , importOccurrenceId, importOccurrenceDescriptor, importOccurrenceSpan
  , CapturedImportSyntax, captureImportSyntax, capturedImportOccurrences, capturedParsedSource
  , DriverImportSyntax, matchDriverImports, driverImportOccurrences, driverParsedModule
  , RefinedImportSyntax, refinedParsedModule, refinedImportOccurrences
  , ImportLineage(..), importOccurrenceLineage
  , ImportSyntaxError(..)
  ) where

import Tidepool.ImportSyntax.Internal
