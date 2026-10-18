module Main where
import Data.Coerce (coerce)
import Tidepool.Actors.Spawn (ContextCheckpoint)
import Tidepool.Model.Selection (OpenAI, Argon)
wrong :: ContextCheckpoint OpenAI -> ContextCheckpoint Argon
wrong = coerce
main :: IO ()
main = pure ()
