{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

-- | Host a text procedure as a resident service with an inspectable ledger.
module Project.Service (Service (..), ServiceEffects, serve) where

import Control.Monad.Freer (Eff, raise)
import qualified Control.Monad.Freer.State as S
import Data.Text (Text)
import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Exomonad
import Tidepool.Effects.Core (Commands, Jev)
import Tidepool.Effects.Row (knownEffects)

data Service mode = Service
  { ledger :: mode :- State [(Text, Text)]
  , ask :: mode :- Call Text (R.Reply Text)
  , history :: mode :- Call () (R.Reply [(Text, Text)])
  } deriving Generic

type ServiceEffects = LocalEffects Service '[Replies, Actor, Notifications, Jev, Commands]

serve :: (Text -> Eff ServiceEffects Text) -> ActorSpec Service ServiceEffects
serve procedure = R.definition "procedure-service" (Actor.Selected knownEffects) Service
  { ledger = []
  , ask = \query -> do
      result <- raise (procedure query)
      S.modify (++ [(query, result)])
      pure result
  , history = \() -> S.get
  }
