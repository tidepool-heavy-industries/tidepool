-- | Representation shared only by the native annotation and import-refinement
-- issuers. Public consumers borrow projections from Tidepool.CheckedCell.
module Tidepool.CheckedCell.Internal (NativeParsedModule(..)) where

import GHC.Types.Name.Set (NameSet)
import Tidepool.ImportSyntax (RefinedImportSyntax)

data NativeParsedModule = NativeParsedModule RefinedImportSyntax NameSet
