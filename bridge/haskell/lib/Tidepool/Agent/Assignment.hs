{-# LANGUAGE DeriveFunctor #-}

-- | Data shared by a child launch and a request to an existing actor.
module Tidepool.Agent.Assignment
  ( Label
  , NameError (..), validateKebabSegment, renderNameError
  , labelFromText
  , labelText
  , Assignment (..)
  , SettlementReporting (..)
  , assignment
  ) where

import Data.String (IsString (fromString))
import Data.Text (Text)
import qualified Data.Text as Text
import Prelude

import Tidepool.Agent.Assignment.Internal (Label, NameError (..), labelFromText, labelText, renderNameError, validateKebabSegment)
import Tidepool.Duration (Duration)
import Tidepool.Effects.Core (Model (..))

instance IsString Model where
  fromString = Alias . Text.pack

data SettlementReporting = NotifyOwner | Silent
  deriving (Show, Eq)

data Assignment input = Assignment
  { assignmentLabel :: Label
  , input :: input
  , guidance :: Maybe Text
  , deadline :: Maybe Duration
  , report :: SettlementReporting
  -- | Other branches admitted in the same 'Tidepool.Actors.Unfold.unfold'
  -- call, as (label, allocated path, truncated preview) triples. Set by
  -- 'Tidepool.Actors.Unfold.requestBranch' immediately before the request is
  -- sent; empty for every other caller of 'assignment'.
  , assignmentSiblings :: [(Text, Text, Text)]
  }
  deriving (Show, Eq, Functor)

assignment :: Label -> input -> Assignment input
assignment name value = Assignment name value Nothing Nothing NotifyOwner []
