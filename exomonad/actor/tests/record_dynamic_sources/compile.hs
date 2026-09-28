{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

module RecordDynamicSources where

import Control.Monad.Freer (Eff)
import GHC.Generics (Generic)
import qualified Tidepool.Actor.Record as R
import Tidepool.Command.Types (Job)
import Tidepool.Effects.Core (Actor, CommandResult)

data Probe mode = Probe
  { probeState :: mode R.:- R.State ()
  , probeDone :: mode R.:- R.Event CommandResult
  } deriving Generic

attachCommand
  :: R.EventSink Probe CommandResult
  -> Job
  -> Eff (R.LocalEffects Probe '[Actor]) (Either R.AttachmentError ())
attachCommand sink job = R.attach sink (R.command job)

fromSelf :: Job -> R.Handler () (R.LocalEffects Probe '[Actor]) ()
fromSelf job = do
  own <- R.self @Probe
  _ <- R.attach (probeDone own) (R.command job)
  pure ()

result :: Int
result = 1
