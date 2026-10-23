module Tidepool.CheckedAdmission
  ( matchesInspectionAdmission
  ) where

import Data.Maybe (isJust)
import qualified Data.Map.Strict as Map
import Tidepool.ExtractRequest (RequestOptions(..), InspectionPayload)

-- An inspection input cap cannot be used by an executable or publication mode.
matchesInspectionAdmission :: RequestOptions -> InspectionPayload -> [String] -> Bool
matchesInspectionAdmission args _inspection values =
  not (optionHarnessProfile args)
    && not (isJust (optionBindGen args))
    && Map.null (optionRetainedGenerations args)
    && optionInjectVals args == values
