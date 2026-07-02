{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, OverloadedRecordDot #-}
-- | Repo-structure verbs: Cargo workspace dependency graph + topological
-- build order, over the Fs effect. buildOrderT self-tests the pure core.
module Repo where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.List as L
import qualified Tidepool.Data.Text as T

-- | The crate name from a Cargo.toml body (the [package] name field).
crateName :: Doc -> Text
crateName d = case [ l | l <- T.lines d.body, T.isPrefixOf "name " l || T.isPrefixOf "name=" l ] of
  (l : _) -> T.filter (\c -> c /= '"' && c /= ' ') (snd (T.breakOnEnd "=" l))
  _       -> ""

-- | In-workspace path-dependency crate names from a Cargo.toml body.
crateDeps :: Doc -> [Text]
crateDeps d =
  [ T.strip (T.takeWhile (\c -> c /= ' ' && c /= '=') l)
  | l <- T.lines d.body
  , T.isPrefixOf "tidepool" (T.strip l)
  , T.isInfixOf "path" l
  ]

-- | The workspace dependency graph: crate -> its in-workspace deps.
-- Uses the Fs primitives (glob/readFile) directly -- readGlob lives in
-- Tidepool.Orchestrate, not Tidepool.Effects, so a lib module can't see it.
crateGraph :: M (Map.Map Text [Text])
crateGraph = do
  paths <- glob "*/Cargo.toml"
  tomls <- mapM (\p -> Doc p <$> readFile p) paths
  let g0    = Map.fromList [ (crateName d, crateDeps d) | d <- tomls, crateName d /= "" ]
      names = Map.keysSet g0
  pure (Map.map (filter (`Set.member` names)) g0)

-- | Kahn topological order; leftover nodes (dev-dep cycles) tagged.
topoOrder :: Map.Map Text [Text] -> [Text]
topoOrder = go []
  where
    go acc done
      | Map.null done = reverse acc
      | otherwise = case [ n | (n, ds) <- Map.toList done, all (`elem` acc) ds ] of
          []      -> reverse acc ++ map ("CYCLE:" <>) (Map.keys done)
          (r : _) -> go (r : acc) (Map.delete r done)

-- | Build order for the live workspace (leaves first).
buildOrder :: M [Text]
buildOrder = topoOrder <$> crateGraph

-- | Self-test: topoOrder places deps before dependents on a known graph.
buildOrderT :: Bool
buildOrderT =
  let g = Map.fromList [("a", []), ("b", ["a"]), ("c", ["a", "b"])]
      o = topoOrder g
      before x y = case (L.elemIndex x o, L.elemIndex y o) of { (Just i, Just j) -> i < j; _ -> False }
  in before "a" "b" && before "b" "c" && before "a" "c" && not (any (T.isPrefixOf "CYCLE") o)
