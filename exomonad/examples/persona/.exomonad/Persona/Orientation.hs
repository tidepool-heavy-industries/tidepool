{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
-- | The context policy used on arrival. This is authored persona code: change
-- the view, occasion or effects here as your way of remembering develops.
module Persona.Orientation (Policy(..), arrival, prepare, inspection, startup) where

import Prelude
import Control.Monad.Freer (Eff, Members)
import Data.Text (Text)
import Tidepool.Agent.Contract (StartupInput(..), ContextSeed, contextSection)
import Tidepool.Aeson (Value, object, (.=))
import Tidepool.Aeson.Value (encodeValue)
import Tidepool.Effects.Core (FsRead, FsWrite, Jev)
import qualified Persona.Attention as A
import qualified Persona.Collection as Collection
import qualified Persona.Context as Context
import qualified Persona.Memory as Memory

data Policy = Policy
  { occasion :: Text
  , limits :: A.Limits
  , attentionView :: A.Collection -> A.View
  }

-- No first human message is available at startup. Later notebook calls can
-- supply a situated occasion and compose a different view over the same sources.
arrival :: Policy
arrival = Policy
  { occasion = "Prepare a general orientation for this fresh conversation from available memory."
  , limits = A.Limits 1 16384
  , attentionView = A.continuity
  }

prepare :: Members '[FsRead, FsWrite, Jev] effects
  => Policy -> Eff effects (Either Context.PreparationError (Collection.Loaded, A.Prepared))
prepare policy = Context.prepare (limits policy) (occasion policy) (attentionView policy)

-- | Inspect or save the same evidence before encoding it at the runtime boundary.
inspection :: StartupInput -> Policy -> Collection.Loaded -> A.Prepared -> Value
inspection input policy loaded prepared = object
  [ "operation" .= startupOperation input
  , "occasion" .= occasion policy
  , "manifest" .= Collection.manifestObservation loaded
  , "initialisation" .= show (Collection.initialisation loaded)
  , "attention" .= A.snapshot prepared
  ]

startup :: Members '[FsRead, FsWrite, Jev] effects
  => StartupInput -> Eff effects ContextSeed
startup input = do
  result <- prepare arrival
  case result of
    Left problem -> error (show problem)
    Right (loaded, prepared) -> do
      -- This note is the latest preparation, not an archive or a publication
      -- receipt. The operation identifies it if later publication fails.
      saved <- Memory.note "initial-attention" $ encodeValue $
        inspection input arrival loaded prepared
      case saved of
        Left problem -> error (show problem)
        Right () -> pure $ contextSection "persona attention"
          (Just "Persona.Orientation.startup; inspection: notes/initial-attention.md")
          (A.rendered prepared)
