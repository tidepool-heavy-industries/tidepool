{-# LANGUAGE OverloadedStrings #-}

-- | Recursion-scheme fork combinators (Wave C — TARGET.md §1 D1 ruling: the
-- agent-facing parallel surface is park-at-fork plus recursion-scheme
-- combinators, the LspGraph idiom — scheme traverses, forked answerers
-- judge). Pure composition over 'returnControlFanout' (F3) — no new effect
-- verbs, no new 'Ask' constructors.
--
-- __Only 'forkFilter' ships here.__ 'forkMap'/'forkCata' as specced
-- (@(a -> Text) -> [a] -> M [b]@, a CALLER-chosen answer type @b@) are
-- BLOCKED by an extraction-pipeline wall, empirically confirmed, not a
-- pragma-tuning gap:
--
--   * Extract statically rejects a @returnControlFanout@ occurrence whose
--     answer type still carries a free type variable (\"polymorphic
--     returnControl site\" —
--     @Tidepool.Translate.checkReturnControlType@).
--   * A caller-generic wrapper's own body necessarily has that type
--     variable free (it's universally quantified over @b@) — the ONLY way
--     to close it is for the wrapper's definition to be duplicated
--     (type-substituted) into each call site before extract ever sees the
--     Core.
--   * That duplication never happens here. Verified two ways against the
--     real extract binary: (1) @{-\# INLINE forkMap \#-}@ — dumped with
--     @TIDEPOOL_DUMP_CLOSED@, the call site (\"result\"'s own closed
--     binding) shows an ordinary, un-inlined @forkMap \@Int \@Int (...)@
--     application; \"forkMap\" survives as its OWN separately-translated
--     closed binding, with @b@ still free, and extract rejects it. (2) An
--     explicit @{-\# SPECIALIZE forkMap :: (Int -> Text) -> [Int] -> M
--     [Int] \#-}@ pragma at the call site — same failure, same site
--     description. Root cause: @runPipeline@ concatenates ALREADY
--     GHC-compiled modules' Core for translation (Translate.hs's #313
--     comment) — each module compiles once, separately; there is no
--     whole-program re-simplification pass afterward for either mechanism
--     to hook into.
--   * 'forkFilter' has no such requirement — its fanout always answers at
--     a fixed 'Bool' regardless of caller, so its own (single, shared)
--     definition is already monomorphic where it calls
--     'returnControlFanout'.
--
-- Closing this for 'forkMap'\/'forkCata' needs either a NEW extract-level
-- mechanism (a Translate.hs change recognizing a library combinator by
-- name, mirroring how 'returnControl'\/'returnControlFork'\/
-- 'returnControlFanout' themselves are recognized — out of THIS spec's
-- boundary) or answering at 'Tidepool.Aeson.Value.Value' (concrete,
-- caller-independent — the same trick 'ask'\/'llm' already use) at the
-- cost of the GHC-verbatim typed-answer retry this feature otherwise
-- promises. Flagged for the parent rather than decided unilaterally here.
-- The @Data.Tree@ wrapper 'forkCata' would have needed (collision-free
-- @node@\/@leaf@ smart constructors — 'Data.Tree.Node' collides on the
-- unqualified name with freer-simple's continuation 'Node', always in
-- scope in an eval) is dropped along with it rather than shipped unused.
module Tidepool.Fork
  ( forkFilter
  ) where

import Prelude
import Data.Text (Text)

import Tidepool.Effects (M, returnControlFanout)

-- | One fanout over 'Bool' verdicts; keep the elements whose verdict is
-- 'True', in the original order.
forkFilter :: (a -> Text) -> [a] -> M [a]
forkFilter mkPrompt xs = do
  verdicts <- returnControlFanout (map mkPrompt xs)
  pure (map fst (filter snd (zip xs verdicts)))
