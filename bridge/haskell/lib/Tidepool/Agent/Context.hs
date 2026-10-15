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
  , ContextTextSelector (..)
  , ContextVisibleText (..)
  , ContextBlockKind (..)
  , ContextProvenance (..)
  , Effort
  , ForkEffort (..)
  , contextBlocks
  , editableTexts
  , visibleTexts
  , referencedTexts
  , ContextEditError (..)
  , removeText
  , trimText
  , blockKind
  , blockProvenance
  , toNotes
  , InitialContextIdentity (..)
  , InitialContextPublicationReceipt (..)
  , InitialContextObservation (..)
  , getInitialContext
  , getContext
  , putContext
  , modifyContext
  , modifyContextM
  , setNextModel
  , setNextEffort
  )
where

import Control.Lens (Fold, Lens', Traversal', folding, lens)
import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import qualified Data.Text as Text
import Prelude

import Tidepool.Inspection.Display (Display (..), displayRecord)
import Tidepool.Inspection.Tree (treeParts)
import Tidepool.Effects.Core
  ( ContextBlock (..)
  , ContextDocument (..)
  , ContextNativeKind (..)
  , ContextTextSelector (..)
  , ContextVisibleText (..)
  , ContextReadWrite (..)
  , InitialContextIdentity (..)
  , InitialContextPublicationReceipt (..)
  , InitialContextObservation (..)
  , ForkEffort (..)
  , ContextReference
  , ContextRole (..)
  )

-- | A transcript copy with no capability-bearing operations or constructor.
newtype Context = Context ContextDocument

-- | A bounded structural view of the visible transcript. References and
-- provenance are data only; this view adds no access to the items they identify.
instance Display Context where
  displayTree (Context (ContextDocument contextBlocksValue)) =
    displayRecord 0 "Context" [("blocks", treeParts "[" "]" (map displayBlock contextBlocksValue))]
    where
      displayBlock (Text _ role body _) =
        displayRecord 0 "Text" [("role", displayTree role), ("body", displayTree body)]
      displayBlock (Native _ kind preview protected texts) =
        displayRecord
          0
          "Native"
          [ ("kind", displayTree kind)
          , ("preview", displayTree preview)
          , ("protected", displayTree protected)
          , ("texts", treeParts "[" "]" (map displayVisibleText texts))
          ]
      displayVisibleText visibleText =
        displayRecord
          0
          "ContextVisibleText"
          [ ("selector", displayTree (contextVisibleTextSelector visibleText))
          , ("editable", displayTree (contextVisibleTextEditable visibleText))
          , ("removable", displayTree (contextVisibleTextRemovable visibleText))
          ]

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

-- | Traverse authored text and full native visible fields marked editable.
-- Native previews, selectors, references, and non-editable fields are kept.
editableTexts :: Traversal' Context Text
editableTexts action (Context document) =
  (\updated -> Context (document {blocks = updated})) <$> traverse edit (blocks document)
  where
    edit (Text reference role body sources) =
      (\updated -> Text reference role updated sources) <$> action body
    edit (Native reference kind preview protected texts) =
      (\updated -> Native reference kind preview protected updated)
        <$> traverse editVisible texts
    editVisible visibleText
      | contextVisibleTextEditable visibleText =
          (\updated -> visibleText {contextVisibleTextText = updated})
            <$> action (contextVisibleTextText visibleText)
      | otherwise = pure visibleText

-- | Visible authored text and full visible native fields in transcript order.
visibleTexts :: Fold Context Text
visibleTexts = folding (concatMap visible . blocks . unwrap)
  where
    unwrap (Context document) = document
    visible (Text _ _ body _) = [body]
    visible (Native _ _ _ _ texts) = map contextVisibleTextText texts

-- | Issued text identities in transcript order. A newly staged text block has
-- no reference until publication; read it again after the tool result.
referencedTexts :: Fold Context ContextVisibleText
referencedTexts = folding (concatMap referenced . blocks . unwrap)
  where
    unwrap (Context document) = document
    referenced (Text (Just reference) _ body _) =
      [ContextVisibleText reference (MessageText 0) body True True]
    referenced (Text Nothing _ _ _) = []
    referenced (Native _ _ _ _ texts) = texts

data ContextEditError
  = ReferenceNotVisible
  | ReferenceAmbiguous
  | RemovalNotAllowed
  deriving (Show, Eq)

-- | Remove one exact issued text occurrence. Native evidence may contain
-- independently authored notes; only fields issued as removable can be omitted.
-- This edits data only. 'putContext' still validates and stages publication.
removeText :: ContextReference -> Context -> Either ContextEditError Context
removeText selected (Context document) = case matches of
  [] -> Left ReferenceNotVisible
  [False] -> Left RemovalNotAllowed
  [True] -> Right (Context (document {blocks = concatMap remove (blocks document)}))
  _ -> Left ReferenceAmbiguous
  where
    matches = concatMap matching (blocks document)
    matching (Text (Just reference) _ _ _) | reference == selected = [True]
    matching (Native _ _ _ _ texts) =
      [contextVisibleTextRemovable field | field <- texts, contextVisibleTextReference field == selected]
    matching _ = []
    remove block@(Text (Just reference) _ _ _) | reference == selected = []
                                             | otherwise = [block]
    remove (Native reference kind preview protected texts) =
      [Native reference kind preview protected (filter ((/= selected) . contextVisibleTextReference) texts)]
    remove block = [block]

-- | Mark retained context text with a short, author-supplied explanation.
-- The marker is ordinary text and has no runtime control meaning.
trimText :: Text -> Text -> Text
trimText reason retained = "[Trimmed: " <> reason <> "]\n" <> retained

blockKind :: ContextBlock -> ContextBlockKind
blockKind (Text _ role _ _) = AuthoredText role
blockKind (Native _ kind _ _ _) = NativeEvidence kind

blockProvenance :: ContextBlock -> ContextProvenance
blockProvenance (Text reference _ _ sources) = AuthoredProvenance reference sources
blockProvenance (Native reference _ _ _ _) = NativeProvenance reference

-- | Replace selected, editable completed exchanges with authored notes. Each
-- note cites the exchange it preserves. Pending, opaque, and protected native
-- blocks remain unchanged.
toNotes :: [ContextReference] -> Context -> Context
toNotes selected (Context document) =
  Context (document {blocks = map asNote (blocks document)})
  where
    asNote block@(Native reference CompletedExchange _ protected texts)
      | reference `elem` selected && not protected && any contextVisibleTextEditable texts =
          Text Nothing User (Text.intercalate "\n" retainedTexts) [reference]
      | otherwise = block
      where
        retainedTexts =
          [ contextVisibleTextText visibleText
          | visibleText <- texts
          , contextVisibleTextEditable visibleText
          ]
    asNote block = block

-- | Observe startup's retained result and historical publication receipt.
-- This performs no startup effects and does not claim its text is still active.
-- The receipt is historical: inspect current context by its exact reference
-- before refreshing or replacing that block. Other authored notes are independent.
-- 'InitialContextPublished' identifies one exact authored block: read current
-- context and replace that reference, refusing a missing target. Editing an
-- existing block preserves its reference through the context owner's provenance.
getInitialContext :: Member ContextReadWrite effects => Eff effects InitialContextObservation
getInitialContext = send GetInitialContextWith

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

-- | Existing reasoning-effort levels used for provider requests.
type Effort = ForkEffort

-- | Stage the actor's reasoning effort for its next request. It is committed
-- only when the whole synchronous cell succeeds.
setNextEffort :: Member ContextReadWrite effects => Effort -> Eff effects ()
setNextEffort = send . SetNextEffortWith
