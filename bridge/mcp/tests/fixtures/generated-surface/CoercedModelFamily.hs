module Main where
import Prelude
import Data.Coerce (coerce)
import Tidepool.Effects.Core
invalid :: FamilyWitness Argon
invalid = coerce DeepSeekWitness
main :: IO ()
main = pure ()
