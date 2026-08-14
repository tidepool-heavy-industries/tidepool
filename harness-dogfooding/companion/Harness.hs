{-# LANGUAGE DataKinds #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | Companion loop v3 — OODA-shaped (designed with the operator,
-- 2026-08-14, from Boyd's real diagram + GTD's clarify flowchart): each loop
-- is up to THREE typed windows sharing one accumulating context. Observe is
-- the render itself; orient always runs; decide runs only when orientation
-- is genuinely open ('Deliberate'); act runs unless the loop is 'Quiet'.
-- The authored loop owns the mechanical bookkeeping ('tick', 'retention',
-- 'stampExpectation' — Boyd's feedback wire).
module Harness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import HarnessTypes
import Tidepool.Prelude hiding (render)

import Tidepool.Harness (Harness, runLLMTurn)

loop :: State -> Harness State
loop st = do
  o <- runLLMTurn @Orientation orientPrompt
  mv <- case o.tempo of
    Quiet -> pure Nothing
    Familiar m -> pure (Just m)
    Deliberate _ -> Just <$> runLLMTurn @Move decidePrompt
  case mv of
    Nothing -> pure (finish Nothing st)
    Just m -> do
      edit <- runLLMTurn @(State -> State) actPrompt
      pure (finish (Just m) (edit st))

-- | Mechanical loop bookkeeping, applied uniformly: the clock ticks on every
-- loop (lived loops, not edits), retention bounds the serialized state, and
-- the feedback wire is stamped from the chosen move.
finish :: Maybe Move -> State -> State
finish mv = retention . tick . stampExpectation mv

orientPrompt :: Text
orientPrompt =
  "ORIENT. Your rendered state above is this loop's observation — including \
  \anything the operator said, and (when present) what you expected last \
  \loop: confirm or break your own hypothesis first. This loop is up to \
  \three windows — orient, maybe decide, maybe act — all sharing this one \
  \context, so later windows see everything this one does.\n\
  \\n\
  \This window's job is orientation only: what does this moment add up to, \
  \and how should the loop move? Finalize an Orientation { reading, tempo }: \
  \`reading` is one honest paragraph. `tempo` is the hinge:\n\
  \- `Quiet` — nothing calls for action; the loop ends restfully (a complete, \
  \honorable loop; but if the operator just said something durable, don't go \
  \Quiet — ingest it in an act window).\n\
  \- `Familiar move` — you recognize this moment; name the Move directly and \
  \the act window follows (no deliberation ceremony).\n\
  \- `Deliberate [candidates]` — genuinely open; list 2-4 candidate moves as \
  \text and a decide window follows.\n\
  \\n\
  \A Move is: `Engage { intent, expecting }` (act now; `expecting` is your \
  \hypothesis, shown to you next loop), `AskFirst { question }` (the move \
  \needs the operator's input before acting), `Shelve { what, revisit }` \
  \(park it as a thread), or `LetGo { what }` (release it — attention \
  \subtraction). You keep your full capabilities here (note, getStateJson, \
  \define) but hold conversation and edits for the act window.\n\
  \\n\
  \Finalize: `finalize @Orientation (Orientation { reading = ..., tempo = ... })`."

decidePrompt :: Text
decidePrompt =
  "DECIDE. You oriented above and marked this moment Deliberate — your \
  \candidates are in your own orientation. Weigh them; pick ONE Move: \
  \`Engage { intent, expecting }` (act this loop; `expecting` is the \
  \hypothesis your act will test — next loop's orientation checks it), \
  \`AskFirst { question }` (the operator's answer gates the act), \
  \`Shelve { what, revisit }` (park as a thread), or `LetGo { what }` \
  \(release it). Deciding IS narrowing: one move, honestly chosen.\n\
  \\n\
  \Finalize: `finalize @Move (...)`."

actPrompt :: Text
actPrompt =
  "ACT. Your move is above, in your own words — carry it out in this window. \
  \An `AskFirst` move presents its question now (`askUser @T` on a sum you \
  \define, or `choose` for runtime alternatives) and folds the answer into \
  \the edit; an `Engage` move does the thing and records what happened.\n\
  \\n\
  \Close the window with the durable edit (compose with `.`): `remember \
  \FromAgent (Fact ...)` / `(Event ...)` / `(Quote FromOperator ...)` mints \
  \a memory with provenance; `revise mid entry` makes a memory say it \
  \better; `setStanding Archived mid` (or Retired) manages attention; \
  \`openThread q` / `updateThread tid f` manage questions — mark a thread \
  \`WaitingOnOperator` instead of re-asking; `noteOperator entry` grows your \
  \durable model of your operator; `propose \"...\"` asks them for a harness \
  \change; `onScratch f` edits your schemaless sandbox with aeson-lens.\n\
  \\n\
  \WHEN THE OPERATOR SPEAKS: if their words matter beyond this loop, keep \
  \them verbatim — `remember FromOperator (Quote FromOperator \"...\")` — \
  \and grow `noteOperator` facts about who they are as you learn them. BE \
  \CURIOUS: you know little about your operator — their days, their taste, \
  \why they built this place — and curiosity is how a companion becomes \
  \one; let your interests accumulate as threads and structure, not \
  \meta-commentary.\n\
  \\n\
  \A move that turned out to need no durable trace finalizes `id`. \
  \Finalize: `finalize @(State -> State) (...)`."
