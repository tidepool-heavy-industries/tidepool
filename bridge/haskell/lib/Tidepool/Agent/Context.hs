{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE TypeOperators #-}
{-# OPTIONS_GHC -Wno-simplifiable-class-constraints #-}

-- | Read and curate the current actor's retained transcript.
--
-- 'Context' contains data only. Its references are identifiers, not authority
-- to recover an item or to restore a context prefix. The runtime validates all
-- edits against the actor's current transcript and principal.
module Tidepool.Agent.Context
  ( Context
  , ContextBlock (..)
  , ContextReference
  , ContextRole (..)
  , ContextNativeKind (..)
  , ContextBlockKind (..)
  , ContextProvenance (..)
  , contextBlocks
  , editableTexts
  , visibleTexts
  , blockKind
  , blockProvenance
  , toNotes
  , getContext
  , putContext
  , modifyContext
  , modifyContextM
  , setNextModel
  )
where

import Control.Lens (Fold, Lens', Traversal', folding, lens)
import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import Prelude

import Tidepool.Inspection.Display (Display (..), displayRecord)
import Tidepool.Effects.Core
  ( ContextBlock (..)
  , ContextDocument (..)
  , ContextNativeKind (..)
  , ContextReadWrite (..)
  , ContextReference
  , ContextRole (..)
  )

-- | A transcript copy with no capability-bearing operations or constructor.
newtype Context = Context ContextDocument

-- | A bounded structural view of the editable transcript and safe native
-- previews. References and provenance are data only; this view adds no access
-- to the items they identify.
instance Display Context where
  displayTree (Context (ContextDocument contextBlocksValue)) =
    displayRecord 0 "Context" [("blocks", displayTree contextBlocksValue)]

data ContextBlockKind
  = AuthoredText ContextRole
  | NativeEvidence ContextNativeKind
  deriving (Show, Eq)

-- | References that explain a block's origin. A text block's item reference
-- identifies the editable item; its sources identify evidence it preserves.
data ContextProvenance
  = AuthoredProvenance (Maybe ContextReference) [ContextReference]
  | NativeProvenance ContextReference
  deriving (Show, Eq)

contextBlocks :: Lens' Context [ContextBlock]
contextBlocks = lens getBlocks setBlocks
  where
    getBlocks (Context document) = blocks document
    setBlocks (Context document) value = Context (document {blocks = value})

-- | Traverse authored text while preserving its role and provenance.
editableTexts :: Traversal' Context Text
editableTexts action (Context document) =
  (\updated -> Context (document {blocks = updated})) <$> traverse edit (blocks document)
  where
    edit (Text reference role body sources) =
      (\updated -> Text reference role updated sources) <$> action body
    edit native@Native {} = pure native

-- | Visible authored text and safe native previews in transcript order.
visibleTexts :: Fold Context Text
visibleTexts = folding (map visible . blocks . unwrap)
  where
    unwrap (Context document) = document
    visible (Text _ _ body _) = body
    visible (Native _ _ preview _) = preview

blockKind :: ContextBlock -> ContextBlockKind
blockKind (Text _ role _ _) = AuthoredText role
blockKind (Native _ kind _ _) = NativeEvidence kind

blockProvenance :: ContextBlock -> ContextProvenance
blockProvenance (Text reference _ _ sources) = AuthoredProvenance reference sources
blockProvenance (Native reference _ _ _) = NativeProvenance reference

-- | Replace selected, editable completed exchanges with authored notes. Each
-- note cites the exchange it preserves. Pending, opaque, and protected native
-- blocks remain unchanged.
toNotes :: [ContextReference] -> Context -> Context
toNotes selected (Context document) =
  Context (document {blocks = map asNote (blocks document)})
  where
    asNote block@(Native reference CompletedExchange preview protected)
      | reference `elem` selected && not protected =
          Text Nothing User preview [reference]
      | otherwise = block
    asNote block = block

getContext :: Member ContextReadWrite effects => Eff effects Context
getContext = Context <$> send GetContextWith

putContext :: Member ContextReadWrite effects => Context -> Eff effects ()
putContext (Context document) = send (PutContextWith document)

modifyContext
  :: Member ContextReadWrite effects
  => (Context -> Context)
  -> Eff effects Context
modifyContext update = do
  current <- getContext
  let updated = update current
  putContext updated
  pure updated

modifyContextM
  :: Member ContextReadWrite effects
  => (Context -> Eff effects Context)
  -> Eff effects Context
modifyContextM update = do
  current <- getContext
  updated <- update current
  putContext updated
  pure updated

-- | Select the actor's next provider model. The host resolves a configured
-- alias first, then treats an unmatched name as a literal model identifier.
setNextModel :: Member ContextReadWrite effects => Text -> Eff effects ()
setNextModel = send . SetNextModelWith
