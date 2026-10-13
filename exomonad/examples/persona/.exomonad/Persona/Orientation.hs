{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module Persona.Orientation (renderMemory, startup) where

import Prelude
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Agent.Contract (StartupInput, ContextSeed, contextSection)
import Tidepool.Effects.Core (FsRead, FsWrite)
import Tidepool.Records (FsError)
import qualified Persona.Memory as Memory

-- Full authored bodies are carried unchanged. Reading or refreshing never
-- creates a missing memory; only startup initialises the seed files.
renderMemory :: Member FsRead effects => Eff effects (Either FsError Text)
renderMemory = do
  let blocks = [Memory.Persona, Memory.Human, Memory.Curiosities]
  observed <- traverse Memory.recall blocks
  pure $ T.intercalate "\n\n" . zipWith section blocks <$> sequence observed
  where
    section block body = "[Memory file: " <> Memory.path block <> "]\n" <> body

startup :: (Member FsRead effects, Member FsWrite effects)
  => StartupInput -> Eff effects ContextSeed
startup _ = do
  created <- Memory.initialise
  case sequence created of
    Left problem -> error (show problem)
    Right _ -> do
      observed <- renderMemory
      case observed of
        Left problem -> error (show problem)
        Right body -> pure $ contextSection "persona memory"
          (Just "Persona.Orientation.startup; .exomonad/state/memory/{persona,human,curiosities}.md") body
