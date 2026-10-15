{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DerivingStrategies #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | The lookup tool's presentation policy. The Lookup effect owns resolution;
-- selection can only add bounded, explicitly requested related declarations.
module Tidepool.Lookup.Tools
  ( LookupTools (..), LookupArguments (..), LookupReadArguments (..), LookupNavigation (..), LookupCaptureReceipt(..), Selection,
    SelectionOutcome(..), SelectionSkip(..), CandidateId, candidateOrdinal, packedIdentity, PackedCandidate, packedOrdinal, packedOriginal, packedCandidate,
    CandidatePacking(..), CandidateAssignment(..), PackingDecision(..),
    LookupCapture(..), LookupExpansion(..), SelectionAssignment(..), SelectionDecision(..),
    captureWith, captureReport, renderCapture, captureRetained,
    tools, toolsWith, packCandidates,
    renderBatch, renderResult, candidateText,
  ) where

import Prelude hiding (lookup)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON, eitherDecode)
import Tidepool.Aeson.Value (ToJSON, toJSON, encodeValue)
import Tidepool.Inspection (displaySelectable, selectDisplay, displayIdentity, Display(..), DisplayTree(..))
import Tidepool.Lookup.Navigation
import Tidepool.Effects.Core (Console)
import Tidepool.Agent.Contract
import Tidepool.Lookup
import Tidepool.Lookup.Render (renderBatch, renderResult)

newtype LookupArguments = LookupArguments { queries :: [Text] }
  deriving stock (Generic)
  deriving anyclass (FromJSON, JsonSchema)

data LookupNavigation = LookupIndex | LookupSections [LookupCaptureSelection] | LookupReceipt | LookupSelectionTrace
  deriving (Generic, FromJSON, ToJSON, JsonSchema)
data LookupReadArguments = LookupReadArguments
  { retainedActor :: Int, retainedIncarnation :: Int, retainedSlot :: Int
  , requested :: LookupNavigation
  } deriving (Generic, FromJSON, JsonSchema)
data LookupCaptureReceipt = LookupCaptureReceipt
  { lookupCaptureActor :: Int, lookupCaptureIncarnation :: Int, lookupCaptureSlot :: Int
  , lookupCaptureView :: Text
  } deriving (Eq, Show, Generic, FromJSON, ToJSON, JsonSchema)

instance Display LookupCaptureReceipt where
  displayTree receipt = Constructor "LookupCaptureReceipt"
    [("actor", displayTree (lookupCaptureActor receipt)), ("incarnation", displayTree (lookupCaptureIncarnation receipt)),
     ("slot", displayTree (lookupCaptureSlot receipt)), ("view", displayTree (lookupCaptureView receipt)),
     ("read_lookup", TextLeaf "Use this exact identity with LookupIndex, LookupReceipt, LookupSelectionTrace, or LookupSections. Sections carry PrimaryLookup or RelatedLookup, a result/entry address, and optional line ranges.")]

data LookupTools mode = LookupTools
  { lookup :: mode :- Call LookupArguments LookupCaptureReceipt
  , readLookup :: mode :- Call LookupReadArguments Text
  }
  deriving (Generic)

-- | Only identities from the supplied captured pool can propose expansion.
-- The packing owner alone issues the supplied candidate pool.
type Selection effects evidence = [LookupResult] -> [PackedCandidate] -> Eff effects (SelectionOutcome evidence)

data SelectionSkip = EnrichmentDisabled | PrimaryUnavailable | NoCandidates
  deriving (Eq, Show)
data SelectionOutcome evidence = SelectionSkipped SelectionSkip
  | SelectionFailed evidence | SelectionSucceeded evidence [CandidateId]

-- | An ID is reusable only with the same captured view, original position and
-- canonical query/reference. It never grants authority beyond the supplied pool.
data CandidateId = CandidateId Text Int Text (Maybe LookupReference) deriving (Eq, Show)

candidateOrdinal :: CandidateId -> Int
candidateOrdinal (CandidateId _ ordinal _ _) = ordinal

packedOrdinal :: PackedCandidate -> Int
packedOrdinal = candidateOrdinal . packedIdentity

data PackedCandidate = PackedCandidate CandidateId LookupCandidate LookupCandidate
  deriving (Eq, Show)

packedIdentity :: PackedCandidate -> CandidateId
packedIdentity (PackedCandidate identity _ _) = identity
packedOriginal :: PackedCandidate -> LookupCandidate
packedOriginal (PackedCandidate _ original _) = original
packedCandidate :: PackedCandidate -> LookupCandidate
packedCandidate (PackedCandidate _ _ bounded) = bounded

data PackingDecision = CandidatePacked PackedCandidate Int Bool
  | CandidateDuplicate Int | CandidateCapOmitted | CandidateBudgetOmitted Int
  deriving (Eq, Show)
data CandidateAssignment = CandidateAssignment
  { assignmentOrdinal :: Int, assignmentDecision :: PackingDecision }
  deriving (Eq, Show)
data CandidatePacking = CandidatePacking
  { packingAssignments :: [CandidateAssignment], packingCandidates :: [PackedCandidate]
  , packingRemainingScalars :: Int }
  deriving (Eq, Show)
data SelectionDecision = CandidateSelected | SelectionUnknown | SelectionDuplicate | SelectionCapOmitted
  deriving (Eq, Show)
data SelectionAssignment = SelectionAssignment
  { proposedIdentity :: CandidateId, selectionDecision :: SelectionDecision }
  deriving (Eq, Show)
data LookupExpansion = ExpansionNotRequested | ExpansionRequested LookupRequest LookupBatch
  deriving (Eq, Show)
data LookupCapture evidence = LookupCapture
  { captureRequest :: LookupRequest, capturePrimary :: LookupBatch, capturePacking :: CandidatePacking
  , captureSelection :: SelectionOutcome evidence
  , captureSelectionAssignments :: [SelectionAssignment], captureSelected :: [PackedCandidate]
  , captureExpansion :: LookupExpansion }

tools :: (Member Lookup effects, Member Console effects) => LookupTools (AsServerT (Eff effects))
tools = toolFor (executeRetained False (\_ _ -> pure (SelectionSkipped EnrichmentDisabled :: SelectionOutcome ())))

toolsWith :: (Member Lookup effects, Member Console effects, Display evidence)
  => Selection effects evidence -> LookupTools (AsServerT (Eff effects))
toolsWith select = toolFor (executeRetained True select)

toolFor :: Member Console effects => (LookupArguments -> Eff effects LookupCaptureReceipt) -> LookupTools (AsServerT (Eff effects))
toolFor action = LookupTools
  { lookup = presentWith presentDisplay $ tool
      "Resolve missing API details before composing a cell: names, module exports, Haskell types or Exomonad documentation in this actor's scope. Pass {\"queries\":[\"Cmd.run\",\":: Int -> Int\",\"doc workbench\"]}; batch related questions. Search by input/output type when the name is unknown. Prefix type searches with :: and use _ for unknown parts. Qualified names are tried before module exports. During a pending request, queries \"respond\" and \"sessionInput\" reveal its exact reply and assignment types. Each query reports independently; narrow ambiguous or truncated matches. Availability labels describe the current effect row: a found signature alone does not establish that it is callable with your effects or runtime authority. When configured, related declarations and alternatives may be attached; explicitly look up a candidate for fuller detail. doc queries are text for this tool, not notebook Haskell."
      action
  , readLookup = presentWith id $ tool
      "Read the retained lookup by the exact actor/incarnation/slot in LookupCaptureReceipt. Use LookupIndex, LookupReceipt, or LookupSections with PrimaryLookup/RelatedLookup and stable result/entry ordinals, named sections and optional 1-based inclusive line ranges. LookupSelectionTrace shows packing, judgment and exact expansion evidence. Selection preserves order and never refetches."
      (\request -> do
        _ <- selectDisplay (retainedActor request, retainedIncarnation request, retainedSlot request)
          (encodeValue (toJSON (requested request)))
        pure "Read from the original retained lookup; source was not fetched again.")
  }

-- | Capture once. Reporting and navigation consume only these retained facts.
captureWith :: Member Lookup effects => Selection effects evidence -> LookupArguments -> Eff effects (LookupCapture evidence)
captureWith = captureLookup True

captureLookup :: Member Lookup effects => Bool -> Selection effects evidence -> LookupArguments -> Eff effects (LookupCapture evidence)
captureLookup discover select (LookupArguments names) = do
  let primaryRequest = (lookupRequest names) {lookupDiscover = discover}
  original <- lookupRaw primaryRequest
  let packing = packCandidates (lookupView original) (lookupCandidates original)
      candidates = packingCandidates packing
  outcome <- if not discover then pure (SelectionSkipped EnrichmentDisabled)
    else if lookupIssue original /= Nothing || searchDisposition (lookupSearchReceipt original) /= LookupSearchCaptured
      then pure (SelectionSkipped PrimaryUnavailable)
    else if null candidates then pure (SelectionSkipped NoCandidates)
    else select (lookupResults original) candidates
  let proposals = case outcome of SelectionSucceeded _ values -> values; _ -> []
      (assignments, selected) = canonicalSelection candidates proposals
  expansion <- if null selected then pure ExpansionNotRequested else do
    let originals = map packedOriginal selected
        request = LookupRequest [candidateQuery c | c <- originals, candidateReference c == Nothing] False
          (Just (lookupView original)) 0 [reference | c <- originals, Just reference <- [candidateReference c]]
    extra <- lookupRaw request
    pure (ExpansionRequested request extra)
  pure (LookupCapture primaryRequest original packing outcome assignments selected expansion)

canonicalSelection :: [PackedCandidate] -> [CandidateId] -> ([SelectionAssignment], [PackedCandidate])
canonicalSelection candidates = go []
  where
    go selected [] = ([], selected)
    go selected (ordinal : rest) =
      let (decision, next) = case filter ((== ordinal) . packedIdentity) candidates of
            [] -> (SelectionUnknown, selected)
            canonical : _
              | any ((== ordinal) . packedIdentity) selected -> (SelectionDuplicate, selected)
              | length selected >= 4 -> (SelectionCapOmitted, selected)
              | otherwise -> (CandidateSelected, selected ++ [canonical])
          (assignments, chosen) = go next rest
      in (SelectionAssignment ordinal decision : assignments, chosen)

captureRetained :: LookupCapture evidence -> RetainedLookupCapture
captureRetained captured = retainLookupCapture (capturePrimary captured) related
  where related = case captureExpansion captured of
          ExpansionNotRequested -> Nothing
          ExpansionRequested _ batch -> Just batch

renderCapture :: LookupCapture evidence -> Text
renderCapture captured = renderBatch (capturePrimary captured) <> judgmentStatus <> case captureExpansion captured of
  ExpansionNotRequested -> ""
  ExpansionRequested _ batch
    | lookupView batch /= lookupView (capturePrimary captured) -> "\n\nRelated declarations unavailable: captured lookup view changed."
    | searchDisposition (lookupSearchReceipt batch) /= LookupSearchCaptured -> "\n\nRelated declarations unavailable; inspect the retained expansion receipt."
    | otherwise -> renderExtra (map packedOriginal (captureSelected captured)) batch
  where judgmentStatus = case captureSelection captured of
          SelectionFailed _ -> "\n\nRelated selection failed; inspect the retained selection evidence for its typed cause."
          _ -> ""

captureReport :: Display evidence => LookupCapture evidence -> DisplayTree
captureReport captured = Constructor "LookupCapture"
  [("request", TextLeaf (T.pack (show (captureRequest captured)))),
   ("primary", Constructor "PrimaryLookup"
     [("identity", displayTree (lookupIdentity primary)), ("view", displayTree (lookupView primary)),
      ("receipt", TextLeaf (T.pack (show (lookupSearchReceipt primary)))),
      ("issue", displayTree (lookupIssue primary)),
      ("results", TextLeaf (renderBatch primary)),
      ("originalCandidates", Sequence "[" "]" (map candidateTree (lookupCandidates primary)))]),
   ("packing", displayTree (capturePacking captured)), ("selection", displayTree (captureSelection captured)),
   ("selectionAssignments", displayTree (captureSelectionAssignments captured)),
   ("selected", displayTree (captureSelected captured)),
   ("expansion", case captureExpansion captured of
     ExpansionNotRequested -> Constructor "ExpansionNotRequested" []
     ExpansionRequested request batch -> Constructor "ExpansionRequested"
       [("request", TextLeaf (T.pack (show request))), ("identity", displayTree (lookupIdentity batch)),
        ("view", displayTree (lookupView batch)), ("receipt", TextLeaf (T.pack (show (lookupSearchReceipt batch)))),
        ("issue", displayTree (lookupIssue batch)), ("results", TextLeaf (renderBatch batch))])]
  where primary = capturePrimary captured

executeRetained :: (Member Lookup effects, Member Console effects, Display evidence)
  => Bool -> Selection effects evidence -> LookupArguments -> Eff effects LookupCaptureReceipt
executeRetained discover select arguments = do
  evidence <- captureLookup discover select arguments
  let captured = captureRetained evidence
      original = capturePrimary evidence
      index = TextLeaf (renderLookupCaptureIndex captured)
      navigate payload = case eitherDecode payload of
        Left issue -> TextLeaf ("Invalid lookup selection: " <> issue)
        Right LookupIndex -> index
        Right LookupSelectionTrace -> captureReport evidence
        Right LookupReceipt -> Constructor "LookupSearchReceipts"
          [(T.pack (show part), TextLeaf (T.pack (show receipt))) | (part, receipt) <- lookupCaptureReceipts captured]
        Right (LookupSections selections) -> displayTree (selectLookupCaptureMany captured selections)
  handle <- displaySelectable index navigate
  let (actor, incarnation, slot) = displayIdentity handle
  pure (LookupCaptureReceipt actor incarnation slot (lookupView original))

sameCandidate :: LookupCandidate -> LookupCandidate -> Bool
sameCandidate a b = candidateQuery a == candidateQuery b && candidateReference a == candidateReference b

-- | Preserve fair/local-first ordering, counting Unicode scalars. Every original
-- candidate gets an assignment. After the unique-candidate cap, remaining rows
-- are cap omissions without maintaining an unbounded duplicate inventory.
packCandidates :: Text -> [LookupCandidate] -> CandidatePacking
packCandidates view = go (8192 * 4) [] 0 . zip [1..]
  where
    go :: Int -> [(Int, LookupCandidate)] -> Int -> [(Int, LookupCandidate)] -> CandidatePacking
    go remaining _ _ [] = CandidatePacking [] [] remaining
    go remaining _ unique remainingCandidates | unique >= 128 = CandidatePacking
      [CandidateAssignment ordinal CandidateCapOmitted | (ordinal, _) <- remainingCandidates] [] remaining
    go remaining seen unique ((ordinal, candidate) : rest) =
      let duplicate = [previous | (previous, original) <- seen, sameCandidate candidate original]
          bounded = candidate {candidateSummary = T.pack (take 2048 (T.unpack (candidateSummary candidate)))}
          cost = scalarLength (candidateText bounded) + 1024
          packed = PackedCandidate (CandidateId view ordinal (candidateQuery candidate) (candidateReference candidate)) candidate bounded
          (decision, nextRemaining, nextSeen, nextUnique, admitted) = case duplicate of
            previous : _ -> (CandidateDuplicate previous, remaining, seen, unique, [])
            [] | cost > remaining -> (CandidateBudgetOmitted cost, remaining, (ordinal, candidate) : seen, unique + 1, [])
               | otherwise -> (CandidatePacked packed cost (candidateSummary bounded /= candidateSummary candidate),
                   remaining - cost, (ordinal, candidate) : seen, unique + 1, [packed])
          following = go nextRemaining nextSeen nextUnique rest
      in following {packingAssignments = CandidateAssignment ordinal decision : packingAssignments following,
                    packingCandidates = admitted ++ packingCandidates following}

candidateTree :: LookupCandidate -> DisplayTree
candidateTree candidate = Constructor "LookupCandidate"
  [("query", displayTree (candidateQuery candidate)), ("origins", displayTree (candidateOrigins candidate)),
   ("summary", displayTree (candidateSummary candidate)), ("local", displayTree (candidateLocal candidate)),
   ("reference", TextLeaf (T.pack (show (candidateReference candidate))))]

instance Display PackedCandidate where
  displayTree candidate = Constructor "PackedCandidate"
    [("identity", displayTree (packedIdentity candidate)),
     ("original", candidateTree (packedOriginal candidate)),
     ("packed", candidateTree (packedCandidate candidate))]
instance Display PackingDecision where
  displayTree (CandidatePacked candidate cost shortened) = Constructor "CandidatePacked"
    [("candidate", displayTree candidate), ("costScalars", displayTree cost), ("summaryShortened", displayTree shortened)]
  displayTree (CandidateDuplicate ordinal) = Constructor "CandidateDuplicate" [("canonicalOrdinal", displayTree ordinal)]
  displayTree CandidateCapOmitted = Constructor "CandidateCapOmitted" []
  displayTree (CandidateBudgetOmitted cost) = Constructor "CandidateBudgetOmitted" [("costScalars", displayTree cost)]
instance Display CandidateAssignment where
  displayTree assignment = Constructor "CandidateAssignment"
    [("ordinal", displayTree (assignmentOrdinal assignment)), ("decision", displayTree (assignmentDecision assignment))]
instance Display CandidatePacking where
  displayTree packing = Constructor "CandidatePacking"
    [("assignments", displayTree (packingAssignments packing)), ("remainingScalars", displayTree (packingRemainingScalars packing))]
instance Display SelectionSkip where displayTree = TextLeaf . T.pack . show
instance Display evidence => Display (SelectionOutcome evidence) where
  displayTree (SelectionSkipped reason) = Constructor "SelectionSkipped" [("reason", displayTree reason)]
  displayTree (SelectionFailed evidence) = Constructor "SelectionFailed" [("evidence", displayTree evidence)]
  displayTree (SelectionSucceeded evidence ordinals) = Constructor "SelectionSucceeded"
    [("evidence", displayTree evidence), ("proposals", displayTree ordinals)]
instance Display CandidateId where
  displayTree (CandidateId view ordinal query reference) = Constructor "CandidateId"
    [("view", displayTree view), ("ordinal", displayTree ordinal),
     ("query", displayTree query), ("reference", TextLeaf (T.pack (show reference)))]
instance Display evidence => Display (LookupCapture evidence) where displayTree = captureReport
instance Display SelectionAssignment where
  displayTree assignment = Constructor "SelectionAssignment"
    [("ordinal", displayTree (proposedIdentity assignment)), ("decision", TextLeaf (T.pack (show (selectionDecision assignment))))]

candidateText :: LookupCandidate -> Text
candidateText candidate =
  candidateQuery candidate <> maybe "" (\reference -> " (" <> T.pack (show (referenceNamespace reference)) <> ")") (candidateReference candidate) <> "\nFor queries: "
    <> T.intercalate ", " (candidateOrigins candidate)
    <> "\n" <> candidateSummary candidate

renderExtra :: [LookupCandidate] -> LookupBatch -> Text
renderExtra selected batch = case lookupIssue batch of
  Just _ -> "\n\nRelated declarations unavailable; use explicit lookup to retry."
  Nothing ->
    let heading = "\n\nRelated declarations\n"
        recovery = recoveryText selected
        allowance = 2048 * 4 - scalarLength heading - scalarLength recovery
        (shown, omitted) = fit allowance (map addition (lookupResults batch))
    in if null shown && not omitted then ""
       else heading <> T.intercalate "\n\n" shown <> if omitted then recovery else ""
  where
    addition result =
      let origins = concat [candidateOrigins c | c <- selected, candidateQuery c == lookupQuery result]
      in renderResult result <> "\n  related to: " <> T.intercalate ", " origins
    fit _ [] = ([], False)
    fit budget (value : rest)
      | scalarLength value > budget =
          let (shown, _) = fit budget rest in (shown, True)
      | otherwise = let (shown, omitted) = fit (budget - scalarLength value - 2) rest
                    in (value : shown, omitted)

scalarLength :: Text -> Int
scalarLength = length . T.unpack

-- Exact namespace references remain recoverable even when a type and its
-- constructor have the same spelling. This is ordinary notebook Haskell.
recoveryText :: [LookupCandidate] -> Text
recoveryText selected =
  let queries' = [candidateQuery c | c <- selected, candidateReference c == Nothing]
      references = [reference | c <- selected, Just reference <- [candidateReference c]]
      expression = "lookupRaw (LookupRequest " <> T.pack (show queries')
        <> " False Nothing 0 " <> T.pack (show references) <> ")"
      full = "\nAdditional detail omitted. Raw notebook recovery: " <> expression
  in if scalarLength full <= 2048 then full
     else "\nAdditional detail omitted; narrow the original lookup or inspect its raw LookupBatch."
