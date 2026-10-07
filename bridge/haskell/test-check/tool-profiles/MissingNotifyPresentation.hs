{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, TypeOperators #-}
module MissingNotifyPresentation where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

data Tools mode = Tools { probe :: mode :- Notify Text } deriving (Generic)

tools :: Tools (AsServerT (Eff '[]))
tools = Tools (notify "Notice" (const (pure ())))
