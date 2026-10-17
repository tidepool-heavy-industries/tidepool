{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE TypeOperators #-}
{-# OPTIONS_GHC -Wno-simplifiable-class-constraints #-}

-- | Read and curate the current actor's retained transcript.
--
-- 'Context' contains data only. Its references and content versions do not
-- grant authority to recover an item or edit a transcript. The runtime validates all
-- edits against the actor's current transcript and principal.
module Tidepool.Agent.Context
  ( Context
  , ContextBlock (..)
  , ContextReference
  , ContextRole (..)
  , ContextNativeKind (..)
  , ContextTextSelector (..)
  , ContextVisibleText (..)
  , ContextTextAuthor (..)
  , ContextTextAuthorship (..)
  , ContextBlockKind (..)
  , ContextProvenance (..)
  , Backend (..)
  , OpenAIModel (..)
  , Model (..)
  , Reasoning (..)
  , DeepSeekReasoning (..)
  , OpenAIReasoning (..)
  , contextBlocks
  , contextAuthorship
  , editableTexts
  , visibleTexts
  , referencedTexts
  , ContextVersion
  , ContextTextTarget
  , ContextUnitTarget
  , textTargets
  , unitTargets
  , targetReference
  , targetSelector
  , targetText
  , targetVersion
  , unitReference
  , unitVersion
  , replaceText
  , removeUnit
  , appendNote
  , RetainedExchangeReference
  , RetainedExchangeObservation (..)
  , observeRetainedExchange
  , appendRetainedExchange
  , contextPatch
  , ContextPatch
  , ContextChange
  , ContextProblem (..)
  , ContextProblemCode (..)
  , ContextTransaction
  , ContextTransactionReceipt (..)
  , ContextTransactionStatus (..)
  , ContextStageResult (..)
  , OriginalTextObservation (..)
  , ContextPreview (..)
  , previewContext
  , trimText
  , blockKind
  , blockProvenance
  , toNotes
  , InitialContextIdentity (..)
  , InitialContextPublicationReceipt (..)
  , InitialContextObservation (..)
  , getInitialContext
  , getContext
  , stageContext
  , inspectContextTransaction
  , inspectOriginalText
  , setNextModel
  , setNextReasoning
  )
where

import Control.Lens (Fold, Getter, folding, to)
import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import qualified Data.Text as Text
import Prelude

import Tidepool.Inspection.Display (Display (..), displayRecord)
import Tidepool.Inspection.Tree (DisplayTree, treeParts)
import Tidepool.Effects.Core
  ( ContextBlock (..)
  , ContextDocument (..)
  , ContextInspection (..)
  , ContextTextVersion (..)
  , ContextUnitVersion (..)
  , ContextPatch (..)
  , RetainedExchangeReference
  , RetainedExchangeObservation (..)
  , ContextChange (..)
  , ContextProblem (..)
  , ContextProblemCode (..)
  , ContextTransaction
  , ContextTransactionReceipt (..)
  , ContextTransactionStatus (..)
  , ContextStageResult (..)
  , OriginalTextObservation (..)
  , ContextNativeKind (..)
  , ContextTextSelector (..)
  , ContextVisibleText (..)
  , ContextTextAuthor (..)
  , ContextTextAuthorship (..)
  , ContextReadWrite (..)
  , InitialContextIdentity (..)
  , InitialContextPublicationReceipt (..)
  , InitialContextObservation (..)
  , Backend (..)
  , OpenAIModel (..)
  , Model (..)
  , Reasoning (..)
  , DeepSeekReasoning (..)
  , OpenAIReasoning (..)
  , ContextReference
  , ContextRole (..)
  )

-- | A transcript copy with no capability-bearing operations or constructor.
newtype Context = Context ContextInspection
  deriving (Show, Eq)

documentOf :: Context -> ContextDocument
documentOf (Context inspection) = contextInspectionDocument inspection

-- | A bounded structural view of the visible transcript. References and
-- provenance are data only; this view adds no access to the items they identify.
instance Display Context where
  displayTree context@(Context inspection) =
    displayRecord 0 "Context"
      [ ("blocks", treeParts "[" "]" (map displayBlock (blocks (documentOf context))))
      , ("textVersions", treeParts "[" "]" (map displayVersion (contextInspectionTextVersions inspection)))
      , ("removableUnits", treeParts "[" "]" (map displayUnit (contextInspectionUnitVersions inspection)))
      , ("authorship", treeParts "[" "]" (map displayAuthorship (contextInspectionAuthorship inspection)))
      ]
    where
      summary body = displayRecord 0 "Text"
        [("characters", displayTree (Text.length body)), ("preview", displayTree (Text.take 160 body))]
      referenceTree value = displayTree (show value)
      displayBlock (Text reference role body sources) =
        displayRecord 0 "Text"
          [("reference", referenceTree reference), ("role", displayTree role),
           ("body", summary body), ("sources", referenceTree sources)]
      displayBlock (Native reference kind preview protected texts) =
        displayRecord 0 "Native"
          [ ("reference", referenceTree reference)
          , ("kind", displayTree kind)
          , ("preview", summary preview)
          , ("protected", displayTree protected)
          , ("texts", treeParts "[" "]" (map displayVisibleText texts))
          ]
      displayVisibleText visibleText = displayRecord 0 "ContextVisibleText"
        [ ("reference", referenceTree (contextVisibleTextReference visibleText))
        , ("selector", displayTree (contextVisibleTextSelector visibleText))
        , ("body", summary (contextVisibleTextText visibleText))
        , ("editable", displayTree (contextVisibleTextEditable visibleText))
        , ("removable", displayTree (contextVisibleTextRemovable visibleText))
        ]
      displayVersion version = displayRecord 0 "ContextTextVersion"
        [ ("reference", referenceTree (contextTextVersionReference version))
        , ("selector", displayTree (contextTextVersionSelector version))
        , ("version", displayTree (Text.take 16 (contextTextVersionDigest version)))
        ]
      displayUnit version = displayRecord 0 "ContextUnitVersion"
        [ ("reference", referenceTree (contextUnitVersionReference version))
        , ("version", displayTree (Text.take 16 (contextUnitVersionDigest version)))
        ]

-- Authorship identifies the immutable canonical original occurrence, keyed by
-- exact field. It does not attribute replacement text to that original author.
displayAuthorship :: ContextTextAuthorship -> DisplayTree
displayAuthorship authorship = displayRecord 0 "ContextTextAuthorship"
  [ ("reference", displayTree (show (contextAuthorshipReference authorship)))
  , ("selector", displayTree (contextAuthorshipSelector authorship))
  , ("originalAuthor", displayTree (show (contextAuthorshipAuthor authorship)))
  , ("authoredNote", displayTree (contextAuthorshipNote authorship))
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

-- | Read-only ordered blocks from the active committed projection.
contextBlocks :: Getter Context [ContextBlock]
contextBlocks = to (blocks . documentOf)

-- | Owner-observed canonical original role/type and authored-note flag for each
-- exact field. Unknown remains explicit. A replacement keeps this original
-- attribution; the editing transaction receipt records the later edit separately.
contextAuthorship :: Getter Context [ContextTextAuthorship]
contextAuthorship = to (\(Context inspection) -> contextInspectionAuthorship inspection)

-- | Eligible text bodies for inspection. Build edits with 'textTargets'.
editableTexts :: Fold Context Text
editableTexts = folding (map targetText . textTargets)

-- | Visible authored text and full visible native fields in transcript order.
visibleTexts :: Fold Context Text
visibleTexts = folding (concatMap visible . blocks . unwrap)
  where
    unwrap = documentOf
    visible (Text _ _ body _) = [body]
    visible (Native _ _ _ _ texts) = map contextVisibleTextText texts

-- | Issued text identities in transcript order. Appended notes receive their
-- reference at commit; read a fresh inspection after the tool result.
referencedTexts :: Fold Context ContextVisibleText
referencedTexts = folding (concatMap referenced . blocks . unwrap)
  where
    unwrap = documentOf
    referenced (Text (Just reference) _ body _) =
      [ContextVisibleText reference (MessageText 0) body True True]
    referenced (Text Nothing _ _ _) = []
    referenced (Native _ _ _ _ texts) = texts

-- | A text and its owner-issued content version. Constructors remain hidden;
-- these are data, and the host revalidates eligibility and current content.
newtype ContextVersion = ContextVersion Text
  deriving (Show, Eq)

data ContextTextTarget = ContextTextTarget ContextTextVersion Text
  deriving (Show, Eq)

newtype ContextUnitTarget = ContextUnitTarget ContextUnitVersion
  deriving (Show, Eq)

textTargets :: Context -> [ContextTextTarget]
textTargets context@(Context inspection) =
  [ ContextTextTarget version (contextVisibleTextText body)
  | version <- contextInspectionTextVersions inspection
  , [body] <- [filter (matches version) (referenced context)]
  , contextVisibleTextEditable body
  ]
  where
    matches version body = contextTextVersionReference version == contextVisibleTextReference body
      && contextTextVersionSelector version == contextVisibleTextSelector body

referenced :: Context -> [ContextVisibleText]
referenced = concatMap fields . blocks . documentOf
  where
    fields (Text (Just reference) _ body _) = [ContextVisibleText reference (MessageText 0) body True True]
    fields (Native _ _ _ _ texts) = texts
    fields _ = []

unitTargets :: Context -> [ContextUnitTarget]
unitTargets (Context inspection) = map ContextUnitTarget (contextInspectionUnitVersions inspection)

targetReference :: ContextTextTarget -> ContextReference
targetReference (ContextTextTarget version _) = contextTextVersionReference version

targetSelector :: ContextTextTarget -> ContextTextSelector
targetSelector (ContextTextTarget version _) = contextTextVersionSelector version

targetText :: ContextTextTarget -> Text
targetText (ContextTextTarget _ body) = body

targetVersion :: ContextTextTarget -> ContextVersion
targetVersion (ContextTextTarget version _) = ContextVersion (contextTextVersionDigest version)

unitVersion :: ContextUnitTarget -> ContextVersion
unitVersion (ContextUnitTarget version) = ContextVersion (contextUnitVersionDigest version)

unitReference :: ContextUnitTarget -> ContextReference
unitReference (ContextUnitTarget version) = contextUnitVersionReference version

-- | Author a provider-visible body while preserving its canonical original.
-- A ReasoningText target changes the inference projection; semantic replay at
-- the live DeepSeek endpoint remains unverified by the offline implementation.
replaceText :: ContextTextTarget -> Text -> ContextChange
replaceText (ContextTextTarget version _) = ReplaceContextText
  (contextTextVersionReference version) (contextTextVersionSelector version) (contextTextVersionDigest version)

removeUnit :: ContextUnitTarget -> ContextChange
removeUnit (ContextUnitTarget version) = RemoveContextUnit
  (contextUnitVersionReference version) (contextUnitVersionDigest version)

-- | Append an assistant note, citing exact evidence references. It cannot
-- synthesize a user message or choose the note's issued reference.
appendNote :: Text -> [ContextReference] -> ContextChange
appendNote = AppendContextNote

-- | Reuse a retained exchange without running its original invocation again.
appendRetainedExchange :: RetainedExchangeReference -> ContextChange
appendRetainedExchange = AppendRetainedExchange

observeRetainedExchange :: Member ContextReadWrite effects => ContextReference -> Eff effects RetainedExchangeObservation
observeRetainedExchange = send . ObserveRetainedExchangeWith

contextPatch :: [ContextChange] -> ContextPatch
contextPatch = ContextPatch

-- | Pure projected document, changed targets, and character counts. This is a
-- local preview, not host admission or a provider token/cache estimate.
-- Inspected authorship is retained observation data from the input, not a new
-- issuance for the proposed document; appended notes have no reference yet.
data ContextPreview = ContextPreview
  { previewDocument :: ContextDocument
  , previewChanges :: [ContextChange]
  , previewCharactersBefore :: Int
  , previewCharactersAfter :: Int
  , previewInspectedAuthorship :: [ContextTextAuthorship]
  } deriving (Show, Eq)

instance Display ContextPreview where
  displayTree preview = displayRecord 0 "ContextPreview"
    [ ("changes", treeParts "[" "]" (map displayChange (previewChanges preview)))
    , ("charactersBefore", displayTree (previewCharactersBefore preview))
    , ("charactersAfter", displayTree (previewCharactersAfter preview))
    , ("document", displayTree (Context (ContextInspection (previewDocument preview) [] [] [])))
    , ("inspectedAuthorship", treeParts "[" "]" (map displayAuthorship (previewInspectedAuthorship preview)))
    ]
    where
      displayChange (ReplaceContextText reference selector _ body) = displayRecord 0 "ReplaceText"
        [("reference", displayTree (show reference)), ("selector", displayTree selector),
         ("characters", displayTree (Text.length body)), ("preview", displayTree (Text.take 160 body))]
      displayChange (RemoveContextUnit reference _) = displayRecord 0 "RemoveUnit"
        [("reference", displayTree (show reference))]
      displayChange (AppendRetainedExchange source) = displayRecord 0 "AppendRetainedExchange"
        [("source", displayTree (show source)), ("resolution", displayTree ("Owner resolves exact retained exchange at commit" :: Text))]
      displayChange (AppendContextNote body sources) = displayRecord 0 "AppendNote"
        [("sources", displayTree (show sources)), ("characters", displayTree (Text.length body)),
         ("preview", displayTree (Text.take 160 body))]

-- | Validate the patch against this inspection's issued versions and fields.
-- Only the host can decide current authority and commit the transaction.
previewContext :: Context -> ContextPatch -> Either [ContextProblem] ContextPreview
previewContext context@(Context inspection) (ContextPatch changes)
  | null problems = Right (ContextPreview after changes (size before) (size after) (contextInspectionAuthorship inspection))
  | otherwise = Left problems
  where
    before = documentOf context
    after = foldl apply before changes
    size = sum . map Text.length . concatMap bodies . blocks
    bodies (Text _ _ body _) = [body]
    bodies (Native _ _ _ _ fields) = map contextVisibleTextText fields
    problem code reference detail = ContextProblem code reference detail
    problems = concatMap validate changes ++ overlaps changes
    validate (ReplaceContextText reference selector expected _) =
      case [v | v <- contextInspectionTextVersions inspection,
            contextTextVersionReference v == reference, contextTextVersionSelector v == selector] of
        [] -> [problem ContextProblemMissing (Just reference) "No issued text version in this inspection"]
        [version]
          | contextTextVersionDigest version /= expected -> [problem ContextProblemStale (Just reference) "Text version differs from this inspection"]
          | null [t | t <- textTargets context, targetReference t == reference, targetSelector t == selector] ->
              [problem ContextProblemIneligible (Just reference) "Text is not uniquely editable in this inspection"]
          | otherwise -> []
        _ -> [problem ContextProblemAmbiguous (Just reference) "Text version is ambiguous"]
    validate (RemoveContextUnit reference expected) =
      case [v | v <- contextInspectionUnitVersions inspection, contextUnitVersionReference v == reference] of
        [] -> [problem ContextProblemMissing (Just reference) "No removable unit version in this inspection"]
        [version] | contextUnitVersionDigest version == expected -> []
                  | otherwise -> [problem ContextProblemStale (Just reference) "Unit version differs from this inspection"]
        _ -> [problem ContextProblemAmbiguous (Just reference) "Unit version is ambiguous"]
    validate (AppendRetainedExchange _) = [] -- Store resolves retained source authority at commit.
    validate (AppendContextNote _ sources) =
      [problem ContextProblemMissing (Just source) "Note source is outside this inspection"
      | source <- sources, source `notElem` knownReferences]
    knownReferences = concatMap references (blocks before)
    references (Text reference _ _ sources) = maybe sources (: sources) reference
    references (Native reference _ _ _ fields) = reference : map contextVisibleTextReference fields
    overlaps [] = []
    overlaps (change : rest) =
      [problem ContextProblemInvalidPatch reference "Patch targets overlap" | other <- rest, conflict change other,
        let reference = changeReference change] ++ overlaps rest
    changeReference (ReplaceContextText reference _ _ _) = Just reference
    changeReference (RemoveContextUnit reference _) = Just reference
    changeReference (AppendContextNote _ _) = Nothing
    changeReference (AppendRetainedExchange _) = Nothing
    conflict (ReplaceContextText a sa _ _) (ReplaceContextText b sb _ _) = a == b && sa == sb
    conflict (RemoveContextUnit a _) (RemoveContextUnit b _) = overlapUnit a b || overlapUnit b a
    conflict (RemoveContextUnit a _) other = maybe False (overlapUnit a) (changeReference other)
    conflict other (RemoveContextUnit b _) = maybe False (overlapUnit b) (changeReference other)
    conflict _ _ = False
    overlapUnit unit reference = unit == reference || any (contains unit reference) (blocks before)
    contains unit reference (Native candidate _ _ _ fields) = candidate == unit
      && reference `elem` map contextVisibleTextReference fields
    contains _ _ _ = False
    apply document (AppendRetainedExchange _) = document
    apply document (AppendContextNote body sources) = document {blocks = blocks document ++ [Text Nothing Assistant body sources]}
    apply document (RemoveContextUnit reference _) = document {blocks = concatMap (remove reference) (blocks document)}
    apply document (ReplaceContextText reference selector _ body) = document {blocks = map (replace reference selector body) (blocks document)}
    remove reference block@(Text candidate _ _ _) = if candidate == Just reference then [] else [block]
    remove reference (Native candidate kind preview protected fields)
      | candidate == reference = []
      | otherwise = [Native candidate kind preview protected (filter ((/= reference) . contextVisibleTextReference) fields)]
    replace reference selector body (Text candidate role original sources)
      | candidate == Just reference && selector == MessageText 0 = Text candidate role body sources
      | otherwise = Text candidate role original sources
    replace reference selector body (Native candidate kind preview protected fields) =
      Native candidate kind preview protected (map update fields)
      where
        update field
          | contextVisibleTextReference field == reference && contextVisibleTextSelector field == selector = field {contextVisibleTextText = body}
          | otherwise = field

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

-- | Replace selected removable completed exchanges with assistant notes.
-- Refuse a missing or ineligible unit rather than silently ignore it.
toNotes :: [ContextReference] -> Context -> Either [ContextProblem] ContextPatch
toNotes selected context = contextPatch . concat <$> traverse asNote selected
  where
    asNote reference = case
      [(unit, fields) | unit <- unitTargets context, unitReference unit == reference,
        Native candidate CompletedExchange _ False fields <- blocks (documentOf context), candidate == reference] of
      [(unit, fields)] -> Right [removeUnit unit, appendNote
        (Text.intercalate "\n" (map contextVisibleTextText (filter contextVisibleTextEditable fields))) [reference]]
      [] -> Left [ContextProblem ContextProblemIneligible (Just reference) "Completed exchange is not uniquely removable"]
      _ -> Left [ContextProblem ContextProblemAmbiguous (Just reference) "Completed exchange is ambiguous"]

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

-- | Stage one batch. A refusal is ordinary data; a staged handle is not a
-- commit receipt. Inspect it in a later cell to observe final settlement.
-- A terminal rejection rolls back this accepted batch and explicit model/effort
-- choices together. A stage refusal accepts no transaction and does not discard
-- separately requested choices or an already accepted batch. Patches themselves
-- never change model or effort; choice-only cells remain supported.
stageContext :: Member ContextReadWrite effects => ContextPatch -> Eff effects ContextStageResult
stageContext = send . StageContextWith

inspectContextTransaction :: Member ContextReadWrite effects => ContextTransaction -> Eff effects ContextTransactionStatus
inspectContextTransaction = send . InspectContextTransactionWith

-- | Observe the exact retained original of an authorized referenced field,
-- including after its removal from the active projection. References do not
-- grant authority, and this does not enumerate the full original history.
inspectOriginalText :: Member ContextReadWrite effects => ContextReference -> ContextTextSelector -> Eff effects OriginalTextObservation
inspectOriginalText reference selector = send (InspectOriginalTextWith reference selector)

-- | Select the actor's next provider model. The host resolves a configured
-- alias first, then treats an unmatched name as a literal model identifier.
setNextModel :: Member ContextReadWrite effects => Model -> Eff effects ()
setNextModel = send . SetNextModelWith

-- | Stage a provider-qualified reasoning choice for the next request.
-- Publication requires the whole synchronous cell to succeed.
setNextReasoning :: Member ContextReadWrite effects => Reasoning -> Eff effects ()
setNextReasoning = send . SetNextReasoningWith
