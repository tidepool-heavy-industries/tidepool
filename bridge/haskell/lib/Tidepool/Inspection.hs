{-# LANGUAGE DataKinds #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE MultiParamTypeClasses #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE UndecidableInstances #-}

-- | Workbench presentation. Summaries describe observations, never acceptance.
module Tidepool.Inspection
  ( Display (..),
    rawText,
    display,
    expand,
    DisplayHandle,
    ExpansionKey,
    expansions,
    WorkbenchDisplay (..),
    GDisplay,
    Rep,
    genericDisplayTree,
    application,
    displayRecord,
    opaqueHandle,
    FullInspection,
    inspectFull,
    DisplayTree (..),
    treeParts,
    precedenceParens,
    DisplayPage,
    text,
    more,
    pageHasMore,
    pageUnavailable,
    PageDisplay (..),
    compactDisplayPage,
    pageWithContinuation,
    emptyPage,
  )
where

import Control.Monad.Freer (Eff, Member, send)
import GHC.Records (HasField (getField))
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Agent.Reply.Internal
import Tidepool.Agent.Watch.Internal
import Tidepool.Inspection.Display
import Tidepool.View.Types (View(..), viewTree)
import Tidepool.View.Wire (encodeView)
import Tidepool.Inspection.Tree
import Tidepool.Effects.Core
  ( Console (DisplayWith, DisplayExpandWith, DisplayAllowanceWith, DisplayExpansionInputWith), displayViewRaw, DirtySummary (..), WorkingState (..), SubmissionObservation (..) )
import Tidepool.Worktree (HeadState (..), renderGitOid, renderWorktreeError)
import Prelude

-- | A value's actor-owned display identity and currently available detail.
-- The constructor is private: runtime authority is checked on every expansion.
data DisplayHandle value = DisplayHandle (Int, Int, Int) [(ExpansionKey, Text)]

expansions :: DisplayHandle value -> [(ExpansionKey, Text)]
expansions (DisplayHandle _ keys) = keys

instance Display (DisplayHandle value) where
  displayTree _ = opaqueHandle "display"

-- | One explicit structured output. The host retains the callback in the
-- actor's ordinary resource scope; the returned handle does not contain it.
display :: (Display value, Member Console effects) => value -> Eff effects (DisplayHandle value)
display value = do
  allowance <- send DisplayAllowanceWith
  let budget = displayBudget allowance
  case displayView value of
    Inspection tree -> do
      let state = newDisplayState budget tree
      identity <- publishDisplay (0, 0, 0) state
      pure (DisplayHandle identity (displayStateKeys state))
    view -> do
      displayViewRaw (encodeView budget view)
      pure (DisplayHandle (0, 0, 0) [])

-- | The key must come from this handle's current expansion description.
-- The host applies the retained callback directly, without compiling a cell.
expand :: Member Console effects => DisplayHandle value -> ExpansionKey -> Eff effects (DisplayHandle value)
expand (DisplayHandle identity _) key = do
  keys <- send (DisplayExpandWith (identity, expansionKeyNumber key))
  pure (DisplayHandle identity [(expansionKeyFromNumber number, label) | (number, label) <- keys])

-- The host grants this allowance before any tree is demanded. Its shared
-- byte budget must never truncate a page after the renderer retains its suffix.
displayBudget :: Int -> Int
displayBudget = max 0 . min 8192

publishDisplay :: forall effects. Member Console effects => (Int, Int, Int) -> DisplayState -> Eff effects (Int, Int, Int)
publishDisplay identity state =
  send (DisplayWith (identity, displayStateText state,
    [(expansionKeyNumber key, label) | (key, label) <- displayStateKeys state], displayStateUnavailable state)
    ((\(_ :: Int) -> do
      (issued, selected, allowance) <- send DisplayExpansionInputWith
      let granted = displayBudget allowance
      case expandDisplayState granted (expansionKeyFromNumber selected) state of
        Nothing -> error "display expansion key is unavailable"
        Just detail -> do
          _ <- publishDisplay issued detail
          pure ()) :: Int -> Eff effects ()))

instance Display ReplyError where
  displayTree ReplyUnauthorized =
    TextLeaf "ReplyUnauthorized (this control operation requires the resource owner or producer; ask that actor to perform it)"
  displayTree ReplyWrongIncarnation =
    TextLeaf "ReplyWrongIncarnation (this handle belongs to a different actor incarnation)"
  displayTree ReplyStale =
    TextLeaf "ReplyStale (the resource is unavailable or this reference is invalid; use a retained value or request fresh work)"
  displayTree error = TextLeaf (Text.pack (show error))

instance Display ResponseFailure where
  displayTree = displayTreePrec 0
  displayTreePrec _ ResponseReleased =
    TextLeaf "ResponseReleased (the response is no longer available; a previously extracted value remains usable)"
  displayTreePrec precedence (ResponseRejected error) =
    application precedence "ResponseRejected" [displayTreePrec 11 error]
  displayTreePrec precedence failure = StringLeaf (showsPrec precedence failure "")

instance Display AwaitError where
  displayTree = displayTreePrec 0
  displayTreePrec precedence (AwaitRejected error) =
    application precedence "AwaitRejected" [displayTreePrec 11 error]
  displayTreePrec precedence failure = StringLeaf (showsPrec precedence failure "")

data FullInspection = FullInspection ([Text] -> Int -> (Text, Bool)) DisplayTree

-- | Compiler-facing retained inspection uses the single Display contract.
inspectFull :: Display a => a -> FullInspection
inspectFull value = FullInspection (\keys budget -> displayWithout keys budget value)
  (viewTree (displayView value))

instance Display FullInspection where
  displayTree (FullInspection _ tree) = tree

instance WorkbenchDisplay FullInspection where
  workbenchDisplay (FullInspection render _) = render [] 65536
  workbenchDisplayWithout keys (FullInspection render _) = render keys 65536
  workbenchActivationDisplay budget (FullInspection render _) = render [] budget

instance WorkbenchDisplay (ResponseResult a) where
  workbenchDisplay value =
    let summary = "ResponseReady · " <> Text.pack (show (responseExecution value))
          <> " · " <> responseWorktreeSummary (responseWorktree value)
        (rendered, _) = rawText 512 summary
     in (rendered, True)

responseWorktreeSummary :: WorktreeEvidence -> Text
responseWorktreeSummary NoBoundWorktree = "no bound worktree evidence"
responseWorktreeSummary (WorktreeObservationFailed failure) =
  "worktree observation failed: " <> renderWorktreeError failure
responseWorktreeSummary (WorktreeObserved _ submitted observation) =
  let working = observation.workingState
      dirty = working.changes
      observedHead = case observation.submittedHead of
        OnBranch _ oid -> oid
        Detached oid -> oid
      counts = Text.pack . show
   in "base=" <> renderGitOid observation.baseHead
      <> " submitted=" <> renderGitOid submitted
      <> " observed=" <> renderGitOid observedHead
      <> " dirty=" <> counts (length dirty.staged) <> "/"
      <> counts (length dirty.unstaged) <> "/"
      <> counts (length dirty.untracked)
      <> " ignored=" <> counts dirty.ignoredExcluded
      <> maybe "" (\operation -> " operation=" <> Text.pack (show operation)) working.operation

-- | The producing actor's lifecycle, provider health, last-activity
-- timestamp and progress revision, plus the wake clause when a registered
-- watch will already resume the caller. All data; a re-poll has nothing to
-- add that this did not already carry.
instance WorkbenchDisplay PendingProgress where
  workbenchDisplay progress =
    ( Text.intercalate " "
        [ "state=" <> Text.pack (show (pendingActorState progress))
        , "health=" <> Text.pack (show (pendingProviderHealth progress))
        , "lastActivityUnixMs=" <> Text.pack (show (pendingLastActivityUnixMs progress))
        , "progressRevision=" <> Text.pack (show (pendingProgressRevision progress))
        ]
        <> wakeClause (pendingWatched progress)
    , False
    )

wakeClause :: Bool -> Text
wakeClause True =
  " · a registered watch wakes this turn when this settles; ending the turn is how to wait for it"
wakeClause False = ""

instance WorkbenchDisplay (ResponseState a) where
  workbenchDisplay (ResponsePending progress) =
    let (text, _) = workbenchDisplay progress
     in ("ResponsePending · " <> text, False)
  workbenchDisplay (ResponseCancellationPending reason) =
    let (text, omitted) = workbenchDisplay reason
     in ("ResponseCancellationPending · " <> text, omitted)
  workbenchDisplay (ResponseReady value) = workbenchDisplay value
  workbenchDisplay (ResponseUnavailable reason) =
    let (text, omitted) = workbenchDisplay reason
     in ("ResponseUnavailable · " <> text, omitted)
  workbenchDisplay (ResponseStarting detail) = ("starting: " <> detail, False)

instance WorkbenchDisplay (WatchState a) where
  workbenchDisplay (WatchPending progress) =
    let (text, _) = workbenchDisplay progress
     in ("WatchPending · " <> text, False)
  workbenchDisplay (WatchReady _) = ("WatchReady", True)
  workbenchDisplay (WatchUnavailable reason) =
    let (text, omitted) = workbenchDisplay reason
     in ("WatchUnavailable · " <> text, omitted)

instance WorkbenchDisplay (ProgressState a) where
  workbenchDisplay ProgressPending = ("ProgressPending", False)
  workbenchDisplay (ProgressUpdate cursor _) =
    ("ProgressUpdate · " <> Text.pack (show cursor), True)
  workbenchDisplay ProgressClosed = ("ProgressClosed", False)
  workbenchDisplay (ProgressRejected reason) =
    let (text, omitted) = workbenchDisplay reason
     in ("ProgressRejected · " <> text, omitted)


-- | A retained display page and an ordinary action that continues its
-- rendering. An exhausted page has an empty, exhausted successor.
-- The representation is a closure so the resident binding owner retains it
-- without deep-forcing the tree or the recursively available future pages.
newtype DisplayPage effects = DisplayPage (() -> (Text, Eff effects (DisplayPage effects), Bool, Bool))

text :: DisplayPage effects -> Text
text (DisplayPage page) = let (value, _, _, _) = page () in value

more :: DisplayPage effects -> Eff effects (DisplayPage effects)
more (DisplayPage page) = let (_, continuation, _, _) = page () in continuation

pageHasMore :: DisplayPage effects -> Bool
pageHasMore (DisplayPage page) = let (_, _, pending, _) = page () in pending

pageUnavailable :: DisplayPage effects -> Bool
pageUnavailable (DisplayPage page) = let (_, _, _, unavailable) = page () in unavailable

instance HasField "text" (DisplayPage effects) Text where
  getField = text

instance HasField "more" (DisplayPage effects) (Eff effects (DisplayPage effects)) where
  getField = more

-- | A page's continuation is an action, not a displayable value; the tree
-- reports only whether one is pending, mirroring 'pageHasMore'.
instance Display (DisplayPage effects) where
  displayTree = displayTreePrec 0
  displayTreePrec precedence page = precedenceParens precedence $ treeParts "DisplayPage {" "}"
    [ Concat [TextLeaf "text = ", displayTree (text page)]
    , Concat [TextLeaf "hasMore = ", TextLeaf (if pageHasMore page then "True" else "False")]
    , Concat [TextLeaf "unavailable = ", TextLeaf (if pageUnavailable page then "True" else "False")]
    ]

emptyPage :: DisplayPage effects
emptyPage = DisplayPage (\() -> ("", pure emptyPage, False, False))

-- | The host supplies the remaining cell allowance. Subsequent pages receive
-- a fresh allowance without rerunning the expression that produced the value.
class PageDisplay effects a where
  displayPage :: Int -> a -> DisplayPage effects

  displayPageWithout :: [Text] -> Int -> a -> DisplayPage effects
  displayPageWithout _ = displayPage

instance {-# OVERLAPPABLE #-} Display a => PageDisplay effects a where
  displayPage budget value = pageWithContinuation budget (viewTree (displayView value)) Nothing

instance PageDisplay effects FullInspection where
  displayPage budget (FullInspection _ tree) = pageWithContinuation budget tree Nothing

-- | A top-level Text or String value pages as raw text, not a quoted literal:
-- the generic instance above renders through 'displayTree', which is
-- 'literalText' for 'Text' and is correct for text nested inside another
-- value, but wrong for a bare top-level result.
instance PageDisplay effects Text where
  displayPage budget value = pageWithContinuation budget (TextLeaf value) Nothing

instance PageDisplay effects [Char] where
  displayPage budget value = pageWithContinuation budget (StringLeaf value) Nothing

instance PageDisplay effects (DisplayPage effects) where
  displayPage budget page =
    let continuation = if pageHasMore page then Just (more page) else Nothing
        rendered = pageWithContinuation budget (TextLeaf (text page)) continuation
    in DisplayPage (\() -> (text rendered, more rendered, pageHasMore rendered,
                            pageUnavailable page || pageUnavailable rendered))

-- | Present a compact first page while retaining the ordinary structural
-- rendering as a continuation whenever the summary omits detail.
compactDisplayPage :: (WorkbenchDisplay a, Display a) => Int -> a -> DisplayPage effects
compactDisplayPage budget value =
  let (summary, omitted) = workbenchDisplay value
      full = if omitted then Just (pure (pageWithContinuation 8192 (displayTree value) Nothing)) else Nothing
   in pageWithContinuation budget (TextLeaf summary) full

instance Display a => PageDisplay effects (ResponseResult a) where
  displayPage = compactDisplayPage

instance Display a => PageDisplay effects (ResponseState a) where
  displayPage = compactDisplayPage

instance Display a => PageDisplay effects (WatchState a) where
  displayPage = compactDisplayPage

instance Display a => PageDisplay effects (ProgressState a) where
  displayPage = compactDisplayPage

pageWithContinuation :: Int -> DisplayTree -> Maybe (Eff effects (DisplayPage effects)) -> DisplayPage effects
pageWithContinuation budget tree continuation =
  let (rendered, remaining, unavailable) = renderTree budget tree
      next = case remaining of
        Just suffix -> pure (pageWithContinuation 8192 suffix continuation)
        Nothing -> maybe (pure emptyPage) id continuation
      pending = case remaining of
        Just _ -> True
        Nothing -> maybe False (const True) continuation
  in DisplayPage (\() -> (rendered, next, pending, unavailable))

instance Display a => Display (ResponseResult a) where
  displayTree = displayTreePrec 0
  displayTreePrec precedence value = precedenceParens precedence $ treeParts "ResponseResult {" "}"
    [ Concat [TextLeaf "responseValue = ", displayTree (responseValue value)]
    , Concat [TextLeaf "responseExecution = ", displayTree (responseExecution value)]
    , Concat [TextLeaf "responseWorktree = ", displayTree (responseWorktree value)]
    ]

instance Display a => Display (ResponseState a) where
  displayTree = displayTreePrec 0
  displayTreePrec precedence (ResponsePending progress) = application precedence "ResponsePending" [displayTreePrec 11 progress]
  displayTreePrec precedence (ResponseCancellationPending reason) = application precedence "ResponseCancellationPending" [displayTreePrec 11 reason]
  displayTreePrec precedence (ResponseReady value) = application precedence "ResponseReady" [displayTreePrec 11 value]
  displayTreePrec precedence (ResponseUnavailable reason) = application precedence "ResponseUnavailable" [displayTreePrec 11 reason]
  displayTreePrec precedence (ResponseStarting detail) = application precedence "ResponseStarting" [displayTreePrec 11 detail]

instance Display a => Display (WatchState a) where
  displayTree = displayTreePrec 0
  displayTreePrec precedence (WatchPending progress) = application precedence "WatchPending" [displayTreePrec 11 progress]
  displayTreePrec precedence (WatchReady value) = application precedence "WatchReady" [displayTreePrec 11 value]
  displayTreePrec precedence (WatchUnavailable reason) = application precedence "WatchUnavailable" [displayTreePrec 11 reason]

instance Display a => Display (ProgressState a) where
  displayTree = displayTreePrec 0
  displayTreePrec _ ProgressPending = TextLeaf "ProgressPending"
  displayTreePrec precedence (ProgressUpdate cursor value) = application precedence "ProgressUpdate" [displayTreePrec 11 cursor, displayTreePrec 11 value]
  displayTreePrec _ ProgressClosed = TextLeaf "ProgressClosed"
  displayTreePrec precedence (ProgressRejected reason) = application precedence "ProgressRejected" [displayTreePrec 11 reason]
