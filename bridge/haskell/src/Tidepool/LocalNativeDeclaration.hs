-- | Private shared proof representation. PlannedDeclaration is its only
-- constructing owner; ExactScope consumes the opaque original association.
module Tidepool.LocalNativeDeclaration
  ( LocalNativeDeclarationAdmission(..), localNativeOwner, localNativeProof ) where

import GHC.Unit.Module (Module, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import Tidepool.FinalizedModuleArtifacts (LocalFinalizedAdmission)

data LocalNativeDeclarationAdmission = LocalNativeDeclarationAdmission
  Module LocalFinalizedAdmission
  deriving (Eq)

instance Show LocalNativeDeclarationAdmission where
  showsPrec precedence native = showsPrec precedence
    (localNativeOwner native, localNativeProof native)

localNativeOwner :: LocalNativeDeclarationAdmission -> (String,String)
localNativeOwner (LocalNativeDeclarationAdmission owner _) =
  (unitString (moduleUnit owner),moduleNameString (moduleName owner))

localNativeProof :: LocalNativeDeclarationAdmission -> LocalFinalizedAdmission
localNativeProof (LocalNativeDeclarationAdmission _ proof) = proof
