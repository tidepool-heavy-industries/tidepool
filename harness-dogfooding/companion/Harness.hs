{-# LANGUAGE DataKinds #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | Companion loop v4 — the OODA windows of v3 (orient always; decide only
-- when orientation is genuinely open; act unless 'Quiet') with the
-- companion-memory answer contract (plans\/companion-memory.md): the act
-- window finalizes a 'Turn' — memory 'Directive's beside the @State ->
-- State@ edit — and the authored loop BATCHES the directives (plus any
-- carried from a failed run) into ONE curator-agent spawn per loop. The
-- receipt carries the store's fresh digest back into 'State'; a failed run
-- is non-fatal (directives carry over, rendered as unfiled).
module Harness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import qualified Data.Text as T
import HarnessTypes
import Tidepool.Agent.Spawn (spawnAgent)
import Tidepool.Effects
  ( SpawnError
  , SpawnOutcome (..)
  , SpawnReceipt (..)
  , WorktreeId (..)
  , fromCurrentRepository
  , renderSpawnError
  , spawnSpec
  , spawnSpecIn
  )
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
    Nothing -> finish Nothing [] st
    Just m -> do
      t <- runLLMTurn @Turn actPrompt
      finish (Just m) t.directives (t.edit st)

-- | Mechanical loop close, applied uniformly: the clock ticks on every loop,
-- the feedback wire is stamped from the chosen move, and this loop's
-- directives (plus any carried unfiled ones) run through the curator.
finish :: Maybe Move -> [Directive] -> State -> Harness State
finish mv ds st0 = do
  let st1 = (tick . stampExpectation mv) st0
      ops = st1.pendingMemOps <> ds
  if null ops
    then pure st1
    else runCurator ops st1 {pendingMemOps = []}

-- | ONE curator spawn for the loop's whole directive batch. First run
-- allocates the store's managed worktree; later runs REBIND the retained one
-- ('spawnSpecIn'). Success carries the fresh digest home and remembers the
-- worktree; failure is non-fatal — the batch carries over (capped) and the
-- render shows it as unfiled.
runCurator :: [Directive] -> State -> Harness State
runCurator ops st = do
  let spec = case st.memWorktree of
        Just wt -> spawnSpecIn (WorktreeId wt) "memory-curator" (curatorBrief ops)
        Nothing -> spawnSpec (fromCurrentRepository "memory-curator") "memory-curator" (curatorBrief ops)
  r <- spawnAgent @MemReceipt spec
  pure
    ( case r of
        Right (outcome, receipt) ->
          st
            { memoryDigest = receipt.digest
            , memWorktree =
                Just (case outcome.outcomeReceipt.receiptWorktree of WorktreeId t -> t)
            , lastCurator =
                Just (receipt.summary <> " [touched: " <> T.intercalate ", " receipt.touched <> "]")
            }
        Left e ->
          st
            { pendingMemOps = take 12 ops
            , lastCurator = Just ("run FAILED (" <> renderSpawnError e <> "); directives carried for retry")
            }
    )

-- | The curator's task text: the ruleset lives in the store (AGENTS.md), the
-- intentions are this batch, the typed result is the receipt.
curatorBrief :: [Directive] -> Text
curatorBrief ops =
  "You are this companion's memory curator. Read AGENTS.md at the repository \
  \root and apply these intentions to the store, then regenerate MEMORY.md \
  \and commit once with a one-line summary of what changed:\n\n"
    <> T.intercalate "\n" (map (("- " <>) . renderDirective) ops)
    <> "\n\nFinalize a result with: digest = the full fresh MEMORY.md contents, \
       \touched = the file paths you changed, summary = your commit message."

orientPrompt :: Text
orientPrompt =
  "ORIENT. Your rendered state above is this loop's observation — including \
  \anything the operator said, your memory store's digest, and (when present) \
  \what you expected last loop: confirm or break your own hypothesis first. \
  \This loop is up to three windows — orient, maybe decide, maybe act — all \
  \sharing this one context, so later windows see everything this one does.\n\
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
  \define — declare the type in one ```haskell block and use it in the next \
  \block of the same reply) and folds the answer into your Turn; an `Engage` \
  \move does the thing and records what happened.\n\
  \\n\
  \Close the window with a Turn { directives, edit }:\n\
  \\n\
  \`directives` are your MEMORY verbs, executed by your curator agent against \
  \your git store after this loop: `Remember \"...\"` files a new fact, \
  \`Modify \"...\"` revises what the store already holds (name the slug from \
  \your digest when you can), `Forget \"...\"` removes it. Prose payloads — \
  \the curator interprets them under the store's own rules. What deserves \
  \remembering: durable facts about your operator, lessons, commitments — \
  \not session ephemera.\n\
  \\n\
  \`edit` is your typed-bag edit (compose with `.`): `openThread q` / \
  \`updateThread tid f` manage questions — mark a thread `WaitingOnOperator` \
  \instead of re-asking; `propose \"...\"` asks the operator for a harness \
  \change; `onScratch f` edits your schemaless sandbox with aeson-lens. \
  \`Turn [] id` is the honest no-change answer.\n\
  \\n\
  \WHEN THE OPERATOR SPEAKS: if their words matter beyond this loop, \
  \`Remember` them (verbatim quotes are fine payloads), and grow the store's \
  \operator model as you learn who they are. BE CURIOUS: you know little \
  \about your operator — their days, their taste, why they built this place — \
  \and curiosity is how a companion becomes one.\n\
  \\n\
  \Finalize with the `:: M ()` annotation, as a bare expression (never a \
  \bind): `(finalize @Turn (Turn { directives = [...], edit = ... }) :: M ())` \
  \— `Turn` carries a function and is deliberately not renderable; the \
  \annotation is what keeps the window's result renderer out of it."
