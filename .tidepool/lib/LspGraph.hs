{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
-- | Multi-hop LSP graph-walk helpers. `lspCallers`/`lspCallees`/`lspRefs` are
-- each `LspNode -> M [LspNode]` (round-2 ergonomics: empty list = no results,
-- never a `Maybe` to unwrap), so a step function composes directly with
-- `concatMapM`/`walk` below without any per-hop unwrapping.
--
-- LAYERING (import direction is strict):
--   Schemes (pure generics)
--     -> verb modules (this one included): effectful vocabularies
--       -> Library (re-export facade; auto-imported into evals)
-- Verb modules import Schemes, never Library (re-export cycle).
module LspGraph where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import Lsp (localCallers, localCallees)
import qualified Data.Set as Set

-- | Identity key for a `LspNode`: file + name + line. A graph walk revisits
-- the same node via multiple paths (diamond edges, cycles), so this is what
-- `dedupNodes`/`walk`'s visited-set key on.
nodeKey :: LspNode -> Text
nodeKey n = nodeFile n <> ":" <> nodeName n <> ":" <> showT (nodeLine n)

-- | Order-preserving dedupe by 'nodeKey'.
dedupNodes :: [LspNode] -> [LspNode]
dedupNodes = go Set.empty
  where
    go _ [] = []
    go seen (n : ns)
      | Set.member (nodeKey n) seen = go seen ns
      | otherwise                   = n : go (Set.insert (nodeKey n) seen) ns

-- | Resolve a name to its ONE workspace definition. Errors loudly (not a
-- `Maybe`/cascade) when the name has zero or 2+ definitions — this is the
-- "skip the list unwrap for a single-def walk" helper, e.g.
-- @named "f" >>= lspCallers@. For LLM-assisted disambiguation among several
-- candidates, use 'Lsp.the'/'Lsp.findDef' instead.
named :: Text -> M LspNode
named name = do
  defs <- lspWhere name >>= liftEither
  case defs of
    []  -> error ("named: no workspace definition found for '" <> name <> "'")
    [n] -> pure n
    ns  -> error
      (  "named: '" <> name <> "' is ambiguous (" <> showT (length ns)
      <> " definitions: " <> intercalate ", "
           [ nodeFile n <> ":" <> showT (nodeLine n) | n <- ns ]
      <> ") — use lspWhere and pick one, or Lsp.the/findDef to disambiguate"
      )

-- | BFS a step function outward from `root` up to `depth` hops, with a
-- visited-set (by 'nodeKey') so cycles and diamond-shaped graphs terminate
-- and never revisit a node. Stops early once a round discovers nothing new.
-- Does NOT include `root` itself in the result.
walk :: (LspNode -> M [LspNode]) -> Int -> LspNode -> M [LspNode]
walk step depth root = go depth (Set.singleton (nodeKey root)) [root]
  where
    go d seen frontier
      | d <= 0 = pure []
      | otherwise = do
          nxt <- concatMapM step frontier
          let fresh = dedupNodes [ n | n <- nxt, not (Set.member (nodeKey n) seen) ]
          if null fresh
            then pure []
            else do
              let seen' = foldl' (\s n -> Set.insert (nodeKey n) s) seen fresh
              rest <- go (d - 1) seen' fresh
              pure (fresh ++ rest)

-- | Full transitive-caller closure: `walk lspCallers` to fixpoint (the
-- visited-set makes cycles cheap and safe; the depth cap is just a
-- backstop, not a real bound in practice).
transitiveCallers :: LspNode -> M [LspNode]
transitiveCallers = walk lspCallers maxBound

-- | Full transitive-callee closure: `walk lspCallees` to fixpoint.
transitiveCallees :: LspNode -> M [LspNode]
transitiveCallees = walk lspCallees maxBound

-- | Workspace-scoped transitive closures: same as 'transitiveCallers'/
-- 'transitiveCallees' but via `Lsp.localCallers`/`localCallees`, which
-- filter to in-workspace nodes before returning each hop. Since the
-- frontier never contains an external node to recurse from, 'walk' stays
-- inside the workspace at every depth -- the right default for any
-- blast-radius/call-graph question, since an unscoped walk floods with
-- stdlib/dependency noise (see haskell/CLAUDE.md's "Known Limits" section).
transitiveLocalCallers, transitiveLocalCallees :: LspNode -> M [LspNode]
transitiveLocalCallers = walk localCallers maxBound
transitiveLocalCallees = walk localCallees maxBound
