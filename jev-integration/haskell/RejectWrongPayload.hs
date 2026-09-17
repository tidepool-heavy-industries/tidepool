module RejectWrongPayload where

import Sketch

-- Must fail: follow handles Edge, not Evidence.
bad :: Steps (Handlers String)
bad = Steps (\(Evidence text) -> text) (\(Evidence text) -> text)
