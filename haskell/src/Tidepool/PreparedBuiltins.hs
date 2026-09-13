-- | Compiler-owned identities, not a spelling-based recovery fallback. These
-- workers have no ordinary recoverable defining type; projection synthesizes
-- callable execution tops while preserving their authoritative saturated arity.
module Tidepool.PreparedBuiltins (wiredInErrorKind) where

import GHC.Builtin.Names qualified as Names
import GHC.Types.Id (Id)
import GHC.Types.Unique (getUnique)
import Tidepool.ExecutionSchema (WiredInErrorKind(..))

wiredInErrorKind :: Id -> Maybe WiredInErrorKind
wiredInErrorKind binder = lookup (getUnique binder)
  [ (Names.patErrorIdKey, WiredPatternMatch)
  , (Names.nonExhaustiveGuardsErrorIdKey, WiredNonExhaustiveGuards)
  , (Names.recSelErrorIdKey, WiredRecordSelector)
  , (Names.recConErrorIdKey, WiredRecordConstruction)
  , (Names.noMethodBindingErrorIdKey, WiredNoMethodBinding)
  , (Names.typeErrorIdKey, WiredDeferredType)
  , (Names.impossibleErrorIdKey, WiredImpossible)
  , (Names.impossibleConstraintErrorIdKey, WiredImpossibleConstraint)
  , (Names.absentErrorIdKey, WiredAbsent)
  , (Names.absentConstraintErrorIdKey, WiredAbsentConstraint)
  , (Names.absentSumFieldErrorIdKey, WiredAbsentSumField)
  ]
