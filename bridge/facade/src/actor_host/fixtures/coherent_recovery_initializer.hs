{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module Recovery.Initializer (startup) where
import Control.Monad.Freer (Eff, Member)
import Tidepool.Agent.Contract (StartupInput, ContextSeed)
import Tidepool.Effects.Core (FsRead, FsWrite)
import qualified Tidepool.Effects.Authored as Files
import qualified Persona.Orientation as Orientation
startup :: (Member FsRead effects, Member FsWrite effects) => StartupInput -> Eff effects ContextSeed
startup input = do
  before <- Files.readFile "initializer-count.txt" >>= either (error . show) pure
  Files.writeFile "initializer-count.txt" (before <> "x") >>= either (error . show) pure
  Orientation.startup input
