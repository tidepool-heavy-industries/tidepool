{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
-- | One pure answer description. Its constructor functions and option payloads
-- remain in Haskell; only controls and occurrence identities cross the wire.
module Tidepool.Form.Algebra (Form(..), Control(..), Option, option, optionView, optionValue, choice, choices, branches, branchesMany, slider, ValidationError(..), textInput, intInput, numberInput, boolInput, present, section, refine, validate, validateForm, optional) where
import Prelude
import Data.Text (Text)
import Data.List.NonEmpty (NonEmpty(..))
import Tidepool.View.Types (View(..))

data Control a where
  TextControl :: Maybe Text -> Control Text
  IntControl :: Maybe Int -> Control Int
  NumberControl :: Maybe Double -> Control Double
  SliderControl :: Double -> Double -> Double -> Control Double
  BoolControl :: Maybe Bool -> Control Bool

data Option a = Option View a
option :: View -> a -> Option a
option = Option
optionView :: Option a -> View
optionView (Option v _) = v
optionValue :: Option a -> a
optionValue (Option _ a) = a

data Form a where
  Pure :: a -> Form a
  Apply :: Form (a -> b) -> Form a -> Form b
  Input :: Text -> Control a -> Form a
  Choice :: Text -> NonEmpty (Option a) -> Maybe Int -> Form a
  Many :: Text -> [Option a] -> [Int] -> Form [a]
  Alternatives :: Text -> NonEmpty (Option (Form a)) -> Maybe Int -> Form a
  AlternativesMany :: Text -> [Option (Form a)] -> Form [a]
  Present :: View -> Form ()
  Section :: Text -> Form a -> Form a
  Refine :: Bool -> (a -> Either [Text] b) -> Form a -> Form b
instance Functor Form where fmap f x = Pure f <*> x
instance Applicative Form where pure = Pure; (<*>) = Apply

data ValidationError = ValidationError { errorField :: Maybe Text, errorMessage :: Text } deriving (Eq, Show)
textInput :: Text -> Maybe Text -> Form Text
textInput label = Input label . TextControl
intInput :: Text -> Maybe Int -> Form Int
intInput label = Input label . IntControl
numberInput :: Text -> Maybe Double -> Form Double
numberInput label = Input label . NumberControl
boolInput :: Text -> Maybe Bool -> Form Bool
boolInput label = Input label . BoolControl
choice :: Text -> NonEmpty (Option a) -> Form a
choice label xs = Choice label xs Nothing
choices :: Text -> [Option a] -> Form [a]
choices label xs = Many label xs []
branches :: Text -> NonEmpty (Option (Form a)) -> Form a
branches label xs = Alternatives label xs Nothing
present :: View -> Form ()
present = Present
section :: Text -> Form a -> Form a
section = Section
-- | Refine a single control locally; composite refinements are form errors.
refine :: (a -> Either Text b) -> Form a -> Form b
refine f = Refine False (either (Left . (:[])) Right . f)
validate :: (a -> [Text]) -> Form a -> Form a
validate f = Refine False (\a -> case f a of [] -> Right a; es -> Left es)
validateForm :: (a -> [Text]) -> Form a -> Form a
validateForm f = Refine True (\a -> case f a of [] -> Right a; es -> Left es)
optional :: Form a -> Form (Maybe a)
optional x = branches "Optional" (option (PlainText "Absent") (pure Nothing) :| [option (PlainText "Present") (Just <$> x)])

-- | A finite bounded range with an explicit initial answer. Invalid authored
-- bounds are refused before any form is mounted.
slider :: Text -> Double -> Double -> Double -> Either Text (Form Double)
slider label lower upper initial
  | any (\x -> isNaN x || isInfinite x) [lower, upper, initial] = Left "Slider bounds and initial value must be finite"
  | lower >= upper = Left "Slider lower bound must be less than its upper bound"
  | initial < lower || initial > upper = Left "Slider initial value is outside its bounds"
  | otherwise = Right (Input label (SliderControl lower upper initial))

-- | Independently reveal and answer zero or more branches. Answers follow
-- authored option order, regardless of the order in which boxes were selected.
branchesMany :: Text -> [Option (Form a)] -> Form [a]
branchesMany = AlternativesMany
