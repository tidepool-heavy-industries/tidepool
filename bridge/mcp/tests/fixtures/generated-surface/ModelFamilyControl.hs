{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
module Main where
import Prelude
import Control.Monad.Freer (Eff, send)
import Tidepool.Effects.Core

type Available = '[ModelCall OpenAI, ModelCall Argon, ForkAgent DeepSeek, SpawnAgent OpenAI]
valid :: Eff Available (Either ModelBoundaryError ())
valid = send (ModelCloseWith ArgonWitness "invocation")
main :: IO ()
main = pure ()
