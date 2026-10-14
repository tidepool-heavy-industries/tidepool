module SwappedCommandObservation where

import qualified Tidepool.Command as Cmd

invalid :: Cmd.Observation
invalid = Cmd.Observation (8192 :: Int) 250
