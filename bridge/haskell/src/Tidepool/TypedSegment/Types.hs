module Tidepool.TypedSegment.Types where

import Control.Exception (Exception)
import qualified Crypto.Hash.SHA256 as SHA256
import qualified Data.ByteString as BS
import Data.Char (isHexDigit)
import Data.List (nub)
import qualified Data.Text as Text
import qualified Data.Text.Encoding as Text
import Data.Word (Word64)
import GHC.Core (CoreBind, CoreExpr, Bind(..))
import GHC.Core.Type (Type)
import GHC.Types.Fixity (Fixity)
import GHC.Types.Id (Id, idType)
import Numeric (showHex)

-- Reservations are inputs to the compiler transformation, not derived from
-- simplified groups. The complete ordered plan has one semantic identity.
data TypedSegmentPlan = TypedSegmentPlan
  { typedSegmentPlanRoot :: String
  , typedSegmentPlanItems :: [TypedItemPlan]
  , typedSegmentReservationDigest :: String
  , typedSegmentPlanDigest :: String
  } deriving (Eq, Show)

data TypedItemPlan = TypedItemPlan
  { plannedItemOrdinal :: Int
  , plannedItemEntry :: String
  , plannedItemGeneration :: Word64
  , plannedItemBody :: TypedItemBody
  } deriving (Eq, Show)

data TypedItemBody
  = ActionItem String String String [String] -- step, probe, marker, authored binders
  | LetItem String [String]                 -- unit sibling marker, complete local group
  | ObservationItem String String          -- occurrence probe, reserved observation binder
  deriving (Eq, Show)

typedSegmentPlan :: String -> String -> [TypedItemPlan]
  -> Either TypedSegmentFailure TypedSegmentPlan
typedSegmentPlan reservation root items = do
  unlessPlan (length reservation == 64 && all isHexDigit reservation) InvalidReservationDigest
  unlessPlan (not (null root) && not (null items)) EmptySegmentPlan
  let ordinals = map plannedItemOrdinal items
      generations = map plannedItemGeneration items
      reserved = root : concatMap reservedNames items
  unlessPlan (all (>= 0) ordinals && unique ordinals) InvalidItemOrdinals
  unlessPlan (all (> 0) generations && unique generations) InvalidItemGenerations
  unlessPlan (all (not . null) reserved && unique reserved) InvalidReservedNames
  unlessPlan (all validBody items) InvalidItemCaptures
  let digest = hex (SHA256.hash (BS.concat (map frame
        (["tidepool-typed-segment-plan-v1", reservation, root] ++ concatMap fields items))))
  pure (TypedSegmentPlan root items reservation digest)
  where
    unique values = length values == length (nub values)
    unlessPlan condition failure = if condition then Right () else Left failure
    reservedNames item = plannedItemEntry item : case plannedItemBody item of
      ActionItem step probe marker _ -> [step, probe, marker]
      LetItem marker _ -> [marker]
      ObservationItem probe observation -> [probe, observation]
    validBody item = case plannedItemBody item of
      ActionItem _ _ _ names -> all (not . null) names && unique names
      LetItem _ names -> all (not . null) names && unique names
      ObservationItem _ _ -> True
    fields item = [show (plannedItemOrdinal item), plannedItemEntry item,
        show (plannedItemGeneration item)] ++ case plannedItemBody item of
      ActionItem step probe marker names -> ["action", step, probe, marker, show (length names)] ++ names
      LetItem marker names -> ["let", marker, show (length names)] ++ names
      ObservationItem probe name -> ["observation", probe, name]
    frame value = let bytes = Text.encodeUtf8 (Text.pack value)
      in Text.encodeUtf8 (Text.pack (show (BS.length bytes) ++ ":")) <> bytes
    hex = concatMap (\byte -> let rendered = showHex byte "" in
      replicate (2 - length rendered) '0' ++ rendered) . BS.unpack

-- Only the successful frontend/extraction owner constructs these products.
-- The item sequence retains its actual enclosing-root predecessor witness.
data TypedSegment = TypedSegment
  { typedSegmentOriginalRoot :: Id
  , typedSegmentItems :: [TypedItem]
  , typedSegmentAuxiliaryRoots :: [CoreBind]
  }

data TypedItem = TypedItem
  { typedItemPlan :: TypedItemPlan
  , typedItemRoot :: Id
  , typedItemInputs :: [TypedItemInput]
  , typedItemActionType :: Type
  , typedItemCaptures :: [TypedCapture]
  , typedItemPredecessor :: Maybe Id
  , typedItemObservation :: Maybe TypedObservation
  , typedItemCore :: CoreExpr
  }

data TypedItemInput = TypedItemInput
  { typedInputParameter :: Id
  , typedInputCapture :: Id
  }

data CaptureOrigin = AuthoredLet | ActionOccurrence | BareObservation
  deriving (Eq, Show)

data TypedCapture = TypedCapture
  { typedCaptureIdentifier :: Id
  , typedCaptureOrigin :: CaptureOrigin
  , typedCaptureFixity :: Maybe Fixity
  }

typedCaptureType :: TypedCapture -> Type
typedCaptureType = idType . typedCaptureIdentifier

data ObservationLift = PureObservation | EffectfulObservation
  deriving (Eq, Show)

-- The original occurrence and the retained thunk have different types.
-- No action/root quantifier is moved into the captured value type.
data TypedObservation = TypedObservation
  { typedObservationIdentifier :: Id
  , typedObservationLift :: ObservationLift
  , typedObservationValueType :: Type
  }

typedSegmentRoots :: TypedSegment -> [CoreBind]
typedSegmentRoots segment =
  [NonRec (typedItemRoot item) (typedItemCore item) | item <- typedSegmentItems segment]
  ++ typedSegmentAuxiliaryRoots segment

data TypedSegmentFailure
  = InvalidReservationDigest
  | EmptySegmentPlan
  | InvalidItemOrdinals
  | InvalidItemGenerations
  | InvalidReservedNames
  | InvalidItemCaptures
  | MissingCaptureRoot
  | AmbiguousCaptureRoot
  | UnprovedFixedUnitRoot
  | UnprovedRootAbstraction
  | UnprovedRootContinuation
  | UnprovedItemSequence Int
  | MissingLetBinder Int String
  | AmbiguousLetBinder Int String
  | IncompleteLetCapture Int
  | UnprovedOccurrenceProbe Int
  | OccurrenceDoesNotUseStep Int
  | UnprovedItemPayload Int
  | UnprovedItemParameters Int
  | ItemDesugaringFailed Int
  | MissingRenamedCaptures
  | ExistingItemRoot Int
  | UnresolvedItemType Int
  | OpenCaptureType Int [String]
  | OpenItemCore Int [String]
  | CaptureCoreLintFailed [String]
  | UnprovedSegmentOperations
  | UnprovedExpressionOccurrence Int
  | WrongObservationEffectRow Int
  | UnprovedCaptureBinding Int
  | UnprovedSegmentGlobals
  | MissingSegmentPreparation
  | SegmentPlanPurposeMismatch
  deriving (Eq, Show)

instance Exception TypedSegmentFailure
