{-# LANGUAGE GADTs #-}
module Main where
import Data.Coerce (coerce)
import Tidepool.Model.Selection (Model, OpenAI, Argon)
wrong :: Model OpenAI -> Model Argon
wrong = coerce
main :: IO ()
main = pure ()
