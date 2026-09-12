module ResidentPreparedProbe where

import Tidepool.Session.Val.G1 (retainedClosure, retainedEnvironment)

residentResult :: Int
residentResult = retainedClosure (sum retainedEnvironment)
