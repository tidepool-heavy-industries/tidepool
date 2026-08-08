{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}

-- | Recursion-scheme fork combinators (Wave C — TARGET.md §1 D1 ruling: the
-- agent-facing parallel surface is park-at-fork plus recursion-scheme
-- combinators, the LspGraph idiom — scheme traverses, forked answerers
-- judge). Pure composition over 'runLLMTurnFanout' (F3) — no new effect
-- verbs, no new 'Ask' constructors.
--
-- __'forkMap'\/'forkCata' (combinator-sites widen).__ These need a
-- CALLER-chosen answer type @b@, which 'forkFilter' (always 'Bool') never
-- did. That used to be a hard extraction-pipeline wall — see the
-- combinator-sites plan's evidence trail — because a caller-generic
-- wrapper's own definition necessarily has @b@ free (universally
-- quantified), and extract rejects a 'runLLMTurnFanout' occurrence whose
-- answer type still carries a free type variable. The wall is closed by a
-- NEW extract-level mechanism (@Tidepool.Translate@ recognizes 'forkMap'\/
-- 'forkCata' by name, exactly like 'runLLMTurn'\/'runLLMTurnFork'\/
-- 'runLLMTurnFanout' themselves), so the answer type is captured at the
-- USER CALL SITE — where the type application is concrete — instead of
-- inside these combinators' own (necessarily still-generic) bodies.
--
-- 'forkMap'\/'forkCata' are therefore OPAQUE stubs, dead at runtime by
-- construction: every well-formed call site gets head-swapped by extract to
-- the @*Sited@ sibling below (mirroring 'runLLMTurn'\/
-- 'runLLMTurnSited'). A call site extract CANNOT rewrite (partial
-- application, or the answer type still a free type variable at that call
-- site) fails AT EXTRACT, naming the site — there is no runtime fallback:
-- each stub's own body DOES call its @*Sited@ sibling (needed so the
-- sibling stays reachable from a real call site — extract's own
-- reachability walk runs over ORIGINAL, pre-interception Core, so it has no
-- way to know a head-swap is coming; the sibling must already be
-- transitively referenced, exactly how 'runLLMTurn' keeps
-- 'runLLMTurnSited' reachable), but the site-id argument it passes is
-- 'error' — a bottom value, forced (as ordinary 'Int' JSON payload data) the
-- instant this path is ever actually reached, so it still fails loudly
-- rather than silently doing the wrong thing.
module Tidepool.Fork
  ( forkFilter
  , fork
  , forkAll
  , forkMap
  , forkMapSited
  , forkCata
  , forkCataSited
  , RoseTree(..)
  ) where

import Prelude
import Data.Text (Text)
import Control.Monad.Freer (Eff, Member)

import Tidepool.Effects (M, RunLLMTurn, runLLMTurnFanout, runLLMTurnFanoutSited, runLLMTurnForkSited)

-- | One fanout over 'Bool' verdicts; keep the elements whose verdict is
-- 'True', in the original order.
forkFilter :: (a -> Text) -> [a] -> M [a]
forkFilter mkPrompt xs = do
  verdicts <- runLLMTurnFanout (map mkPrompt xs)
  pure (map fst (filter snd (zip xs verdicts)))

-- | 'fork' — the SINGLETON parallel sub-answerer: one brief, parked once,
-- answered as a single typed value. Structurally IDENTICAL to
-- 'Tidepool.Effects.runLLMTurnFork' (one type arg, one 'Text' value arg,
-- non-list answer), so extract's @Translate.hs@ recognizes it by name
-- (mirroring 'runLLMTurn'\/'runLLMTurnFork'\/'runLLMTurnFanout'\/'forkAll')
-- and head-swaps every well-formed call site straight to the EXISTING
-- 'runLLMTurnForkSited' sibling — no separate @forkSited@ needed. The answer
-- type @a@ is picked by an explicit type application at the call site
-- (@fork \@T brief@).
--
-- Dead at runtime, same discipline as 'forkAll' (see its haddock): every
-- extractable call site is head-swapped before this body ever runs; the
-- bottom site-id forces an immediate 'error' if reached anyway.
{-# OPAQUE fork #-}
fork :: forall a effs. Member RunLLMTurn effs => Text -> Eff effs a
fork brief = runLLMTurnForkSited unreachableSiteId brief
  where
    unreachableSiteId = error
      "fork: unreachable — extract must head-swap every well-formed \
      \call site to runLLMTurnForkSited; reaching this body means a \
      \call site was not fully applied or its answer type was not \
      \resolved to a concrete type, which extract should already have \
      \rejected"

-- | 'forkAll' — N sub-agents in parallel, @mapConcurrently@-shaped: one
-- prompt per element, parked once, answered together as a typed batch in
-- original order. The answer type @a@ is picked by an explicit type
-- application at the call site (@forkAll \@T prompts@), exactly like a bare
-- 'runLLMTurnFanout' call — 'forkAll'\'s shape is structurally IDENTICAL
-- to 'runLLMTurnFanout'\'s, so extract's @Translate.hs@ recognizes it by
-- name (mirroring 'runLLMTurn'\/'runLLMTurnFork'\/'runLLMTurnFanout')
-- and head-swaps every well-formed call site straight to the EXISTING
-- 'runLLMTurnFanoutSited' sibling — no separate @forkAllSited@ needed.
--
-- Dead at runtime, same discipline as 'forkMap' (see its haddock): every
-- extractable call site is head-swapped before this body ever runs; the
-- bottom site-id forces an immediate 'error' if reached anyway.
--
-- Member-polymorphic (siteid-plugin), MUST MATCH 'runLLMTurnFanoutSited's
-- OWN constraint exactly: extract's head-swap re-applies whatever dictionary
-- arguments the ORIGINAL (this) call site carried, verbatim, to the *Sited
-- sibling it swaps to (see Translate.hs's 'splitTrailingArgs' — it does not
-- (cannot, being untyped at this stage) independently re-derive what
-- constraint the sibling needs). Leaving 'forkAll' fixed to the closed `M`
-- stack while 'runLLMTurnFanoutSited' gained a `Member` constraint broke
-- this silently at RUNTIME (not extraction) — a case-trap tag mismatch, from
-- calling the sibling's now-4-lambda closure with only the 2 args this call
-- site used to supply.
{-# OPAQUE forkAll #-}
forkAll :: forall a effs. Member RunLLMTurn effs => [Text] -> Eff effs [a]
forkAll prompts = runLLMTurnFanoutSited unreachableSiteId prompts
  where
    unreachableSiteId = error
      "forkAll: unreachable — extract must head-swap every well-formed \
      \call site to runLLMTurnFanoutSited; reaching this body means a \
      \call site was not fully applied or its answer type was not resolved \
      \to a concrete type, which extract should already have rejected"

-- | 'forkMap' — one fanout over a CALLER-chosen answer type @b@: build one
-- prompt per element, park once, and answer with the batch of typed
-- verdicts in original order. The answer type @b@ is the FIRST type
-- argument (mirrors a single explicit @forkMap \@T@ application, exactly
-- like 'Tidepool.Effects.runLLMTurn'); the element type @a@ is inferred
-- from the list argument and is never checked at extract — it never
-- crosses the suspend boundary, only @b@ does.
--
-- Dead at runtime: every extractable call site is head-swapped to
-- 'forkMapSited' before this body ever runs (see the module haddock). If
-- reached anyway (an extract bug, or a call this pass genuinely could not
-- rewrite slipping through), the bottom site-id forces an immediate 'error'
-- instead of silently answering wrong.
{-# OPAQUE forkMap #-}
forkMap :: forall b a. (a -> Text) -> [a] -> M [b]
forkMap mkPrompt xs = forkMapSited unreachableSiteId mkPrompt xs
  where
    unreachableSiteId = error
      "forkMap: unreachable — extract must head-swap every well-formed \
      \call site to forkMapSited; reaching this body means a call site \
      \was not fully applied or its answer type was not resolved to a \
      \concrete type, which extract should already have rejected"

-- | The real 'forkMap' logic, reached ONLY via extract's head-swap (a fresh
-- literal site-id prepended at the ORIGINAL 'forkMap' call site — never
-- synthesized here). Routes through exactly ONE 'runLLMTurnFanoutSited'
-- dispatch, so every AskWith payload this site's id ever tags really does
-- carry the same @[b]@-shaped fanout a bare 'runLLMTurnFanout' site
-- would — the asks.json sidecar entry extract records for the ORIGINAL call
-- site describes this dispatch precisely.
{-# OPAQUE forkMapSited #-}
forkMapSited :: forall b a. Int -> (a -> Text) -> [a] -> M [b]
forkMapSited sid mkPrompt xs = runLLMTurnFanoutSited sid (map mkPrompt xs)

-- | Multi-way tree for 'forkCata'. A distinct constructor name from
-- @Data.Tree@'s @Node@ — which collides with freer-simple's always-in-scope
-- continuation @Node@, the reason a 'forkCata' over @Data.Tree.Tree@ was
-- dropped at the recursion-scheme leaf (see the module haddock's evidence
-- trail).
data RoseTree a = RoseTree a [RoseTree a]
  deriving (Show, Eq)

-- | 'forkCata' — a catamorphism over a 'RoseTree': fold bottom-up
-- ("children before parents"), so a node's own prompt sees its children's
-- ALREADY-ANSWERED verdicts. Same answer-type convention as 'forkMap' (the
-- FIRST type argument); see the module haddock for why this needed the same
-- extract-level mechanism 'forkMap' does, and 'forkCataSited' below for how
-- the recursion stays within a single reused effect verb.
--
-- Dead at runtime, same discipline as 'forkMap' — see its haddock.
{-# OPAQUE forkCata #-}
forkCata :: forall b a. (a -> [b] -> Text) -> RoseTree a -> M b
forkCata mkPrompt t = forkCataSited unreachableSiteId mkPrompt t
  where
    unreachableSiteId = error
      "forkCata: unreachable — extract must head-swap every well-formed \
      \call site to forkCataSited; reaching this body means a call site \
      \was not fully applied or its answer type was not resolved to a \
      \concrete type, which extract should already have rejected"

-- | The real 'forkCata' logic, reached ONLY via extract's head-swap (see
-- 'forkMapSited'). A node's DIRECT children are batched into ONE
-- 'runLLMTurnFanoutSited' call — each child's own prompt is built by
-- first recursively resolving ITS children the same way, so a level always
-- fans out only once its dependencies (its own children's verdicts) are
-- ready. This node's own prompt (built from the already-known batch of
-- child verdicts) is then answered as a singleton fanout (fan = 1) — the
-- SAME effect verb at every level of the tree, so every dispatch this
-- site's id ever tags carries a @[b]@-shaped fanout, exactly matching what
-- the asks.json sidecar records for the call site (same discipline as
-- 'forkMapSited').
{-# OPAQUE forkCataSited #-}
forkCataSited :: forall b a. Int -> (a -> [b] -> Text) -> RoseTree a -> M b
forkCataSited sid mkPrompt root = do
  prompt <- promptFor root
  answers <- runLLMTurnFanoutSited sid [prompt]
  case answers of
    [ans] -> pure ans
    _     -> error "forkCataSited: singleton fanout returned a non-singleton batch"
  where
    promptFor :: RoseTree a -> M Text
    promptFor (RoseTree x children) = do
      childAnswers <- case children of
        [] -> pure []
        cs -> do
          childPrompts <- mapM promptFor cs
          runLLMTurnFanoutSited sid childPrompts
      pure (mkPrompt x childAnswers)
