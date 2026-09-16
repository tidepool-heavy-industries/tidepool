{-# LANGUAGE DataKinds #-}
{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE ExistentialQuantification #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE RoleAnnotations #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE ExplicitNamespaces #-}

-- | GHC-only feasibility sketch. No provider codec, effect implementation, or
-- validity claim. Constructors carrying evidence stay private to this module.
module Sketch
  ( Field, type (:-), Option, Choice, Group, Each
  , Descriptions, Probabilities, Handlers, Questions, Answers
  , Steps(..), Walk(..), FollowInfo(..), Edge(..), Evidence(..)
  , ChoiceResult, Selected, Distribution, withChoice, probabilityOf, matchChoice
  , exampleResult
  ) where

import Data.Kind (Type)
import qualified Data.Map.Strict as Map

data Option payload description
data Choice (options :: Type -> Type)
data Group (schema :: Type -> Type)
data Each (schema :: Type -> Type)
data Descriptions
data Probabilities
data Handlers result
data Questions
data Answers

type mode :- endpoint = Field mode endpoint
infixr 0 :-

type family Field mode endpoint where
  Field Descriptions (Option payload description) = (description, payload)
  Field Probabilities (Option payload description) = Double
  Field (Handlers result) (Option payload description) = payload -> result
  Field Questions (Choice options) = options Descriptions
  Field Answers (Choice options) = ChoiceResult options
  Field Questions (Group schema) = schema Questions
  Field Answers (Group schema) = schema Answers
  Field Questions (Each schema) = Map.Map String (schema Questions)
  Field Answers (Each schema) = Map.Map String (schema Answers)

data FollowInfo = FollowInfo { symbol :: String, calls :: [String] }
newtype Edge = Edge Int
newtype Evidence = Evidence String

data Steps mode = Steps
  { follow :: mode :- Option Edge FollowInfo
  , finish :: mode :- Option Evidence String
  }

data Walk mode = Walk
  { step :: mode :- Choice Steps
  , children :: mode :- Each Walk
  }

type role Selected nominal nominal
data Selected (scope :: Type) options = Selected
  (forall result. options (Handlers result) -> result)
  (options Probabilities -> Double)

type role Distribution nominal nominal
newtype Distribution (scope :: Type) options = Distribution (options Probabilities)

data ChoiceResult options = forall scope. ChoiceResult
  (Selected scope options) (Distribution scope options)

withChoice
  :: ChoiceResult options
  -> (forall scope. Selected scope options -> Distribution scope options -> result)
  -> result
withChoice (ChoiceResult chosen distribution) use = use chosen distribution

probabilityOf :: Selected scope options -> Distribution scope options -> Double
probabilityOf (Selected _ project) (Distribution values) = project values

matchChoice :: ChoiceResult options -> options (Handlers result) -> result
matchChoice (ChoiceResult (Selected run _) _) = run

-- An internal synthetic result exercises the eliminators without giving callers
-- a public constructor for scoped evidence or pretending to decode a response.
exampleResult :: ChoiceResult Steps
exampleResult = ChoiceResult
  (Selected (\handlers -> follow handlers (Edge 7)) follow)
  (Distribution (Steps 0.8 0.2))
