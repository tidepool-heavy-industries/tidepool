{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}

-- | Test fixture: the generic-`askUser` PRD's own example types
-- (@plans\/self-iterating-harness\/14-generic-derived-askuser-prd.md@),
-- verbatim.
--
-- The point of the fixture is what is NOT here. @deriving (Generic,
-- FromJSON)@ is the entire author contract — the @FromJSON@ is the vendored
-- generic DEFAULT (no method written), and there is no form builder, no
-- instance of anything in @Tidepool.Form.*@, no annotation beside a field.
-- A turn that compiles @askUser \@DeployRequest@ against this module is the
-- acceptance criterion "declare the ADTs and ask, bare".
module PrdTypes
  ( Environment (..)
  , Destination (..)
  , DeployRequest (..)
  , MaybeUnitField (..)
  ) where

import GHC.Generics (Generic)
import Tidepool.Prelude

data Environment = Development | Staging | Production
  deriving (Generic, FromJSON)

data Destination
  = LocalHost
  | Ssh { host :: Text, port :: Int }
  | Container { image :: Text }
  deriving (Generic, FromJSON)

data DeployRequest = DeployRequest
  { service       :: Text
  , environment   :: Environment
  , destination   :: Destination
  , replicas      :: Int
  , runMigrations :: Bool
  , releaseNote   :: Maybe Text
  }
  deriving (Generic, FromJSON)

-- | Fixture for the Medium-6 `Maybe ()` rejection: `FormRoot`/`GForm`'s
-- `FieldCheck` must reject this field at compile time, naming it and
-- explaining why (`Just ()` and `Nothing` are indistinguishable once
-- collected — both are JSON `null`).
data MaybeUnitField = MaybeUnitField { flag :: Maybe () }
  deriving (Generic, FromJSON)
