{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, TypeOperators #-}
module MissingNestedPresentation where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

data Inner mode = Inner { probe :: mode :- Call Text Text } deriving (Generic)
data Outer mode = Outer { inner :: Inner mode } deriving (Generic)

tools :: Outer (AsServerT (Eff '[]))
tools = Outer (Inner (tool "Echo" pure))
