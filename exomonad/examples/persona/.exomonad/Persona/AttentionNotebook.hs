{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
-- | A worked actor language for revising attention while retaining sources and
-- previous preparation. No actor is started until its caller chooses to do so.
module Persona.AttentionNotebook
  ( Instruction(..), Notebook, Snapshot(..), notebook, snapshot, execute
  , AttentionActor(..), AttentionEffects, definition, framing
  ) where

import Prelude
import Control.Monad (void)
import Control.Monad.Freer (Eff, Members)
import Control.Monad.Freer.State (State, get, put)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Actor.Record ((:-))
import qualified Tidepool.Actor.Record as R
import qualified Tidepool.Actor as Actor
import Tidepool.Effects.Core (Jev)
import Tidepool.Effects.Row (knownEffects)
import qualified Tidepool.Form as F
import Tidepool.Inspection.Display (Display(..))
import Tidepool.Inspection.Tree (DisplayTree(..))
import qualified Persona.Attention as A
import qualified Persona.Context as Context
import qualified Persona.Orientation as O

-- Functions remain Haskell values. A form or another actor can supply the
-- same instructions; no string dispatch or JSON encoding of code is needed.
data Instruction
  = Sources A.Collection
  | View (A.Collection -> A.View)
  | Occasion Text
  | Budget A.Limits
  | Prepare

data Notebook = Notebook A.Collection O.Policy (Maybe (Either A.Problem A.Prepared)) Bool

-- Materialize the current view for inspection; keep the last result separate
-- so editing a draft never relabels an earlier judgment as a fresh one.
data Snapshot = Snapshot
  { sources :: A.Collection
  , occasion :: Text
  , budget :: A.Limits
  , draft :: A.View
  , previous :: Maybe (Either A.Problem A.Prepared)
  , changedSincePreparation :: Bool
  } deriving (Eq, Show)

instance Display Snapshot where
  displayTree value = Constructor "AttentionNotebook"
    [ ("sources", displayTree (sources value))
    , ("occasion", displayTree (occasion value))
    , ("budget", displayTree (budget value))
    , ("draft", displayTree (draft value))
    , ("previous", displayTree (previous value))
    , ("changedSincePreparation", displayTree (changedSincePreparation value))
    ]

notebook :: A.Collection -> O.Policy -> Notebook
notebook collection policy = Notebook collection policy Nothing True

snapshot :: Notebook -> Snapshot
snapshot (Notebook collection policy previousResult changed) = Snapshot
  collection (O.occasion policy) (O.limits policy) (O.view policy collection)
  previousResult changed

-- The same interpreter runs in a notebook cell or a record-actor handler.
-- Only Prepare invokes Jev. Multiple edits can settle before a single judgment.
execute :: Members '[State Notebook, Jev] effects
  => [Instruction] -> Eff effects Snapshot
execute instructions = mapM_ step instructions >> (snapshot <$> get)
  where
    step instruction = do
      Notebook collection policy previousResult _ <- get
      case instruction of
        Sources next -> put (Notebook next policy previousResult True)
        View next -> put (Notebook collection (policy {O.view = next}) previousResult True)
        Occasion next -> put (Notebook collection (policy {O.occasion = next}) previousResult True)
        Budget next -> put (Notebook collection (policy {O.limits = next}) previousResult True)
        Prepare -> do
          result <- Context.prepareView (O.limits policy) (O.occasion policy)
            collection (O.view policy collection)
          put (Notebook collection policy (Just result) False)

data AttentionActor mode = AttentionActor
  { private :: mode :- R.State Notebook
  , run :: mode :- R.Call [Instruction] (R.Reply Snapshot)
  , inspect :: mode :- R.Call () (R.Reply Snapshot)
  , changed :: mode :- R.Event [Instruction]
  } deriving (Generic)

type AttentionEffects = R.LocalEffects AttentionActor '[Jev]

-- Map an existing source into instructions (including Prepare only when
-- useful). Serialized handlers retain the resulting notebook between events.
definition :: R.EventSource [Instruction] -> Notebook
  -> R.ActorSpec AttentionActor AttentionEffects
definition events initial = R.definition "attention-notebook" (Actor.Selected knownEffects)
  AttentionActor
    { private = initial
    , run = execute
    , inspect = const (snapshot <$> get)
    , changed = R.on events (void . execute)
    }

-- The submitted form carries the very same language as the actor endpoint.
-- Dismissal and availability remain FormResult outcomes at the caller.
framing :: Text -> F.Form [Instruction]
framing current = (\next -> [Occasion next, Prepare])
  <$> F.textInput "What should this memory help us attend to?" (Just current)
