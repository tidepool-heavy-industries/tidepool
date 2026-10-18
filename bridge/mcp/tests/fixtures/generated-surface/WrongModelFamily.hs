{-# LANGUAGE DataKinds #-}
module Main where
import Prelude
import Control.Monad.Freer (Eff, send)
import Tidepool.Effects.Core
invalid :: Eff '[ModelCall Argon] (Either ModelBoundaryError ())
invalid = send (ModelCloseWith DeepSeekWitness "invocation")
main :: IO ()
main = pure ()
