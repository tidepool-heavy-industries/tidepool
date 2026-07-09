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
crateName :: Text -> Text
crateName body = case [ l | l <- T.lines body, T.isPrefixOf "name " l || T.isPrefixOf "name=" l ] of
  (l : _) -> T.filter (\c -> c /= '"' && c /= ' ') (snd (T.breakOnEnd "=" l))
  _       -> ""

-- | In-workspace path-dependency crate names from a Cargo.toml body.
-- Section-aware: only the real @[dependencies]@ table, NOT
-- @[dev-dependencies]@ / @[build-dependencies]@ — folding those in creates
-- test-only back-edges (e.g. tidepool-testing → core) that make the graph
-- cyclic and collapse 'topoOrder' to @CYCLE:@.
crateDeps :: Text -> [Text]
crateDeps body =
  [ T.strip (T.takeWhile (\c -> c /= ' ' && c /= '=') l)
  | l <- depsSection body
  , T.isPrefixOf "tidepool" (T.strip l)
  , T.isInfixOf "path" l
  ]
  where
    -- Lines from just after the [dependencies] header to the next [section].
    depsSection = takeWhile (not . T.isPrefixOf "[" . T.strip)
                . drop 1
                . dropWhile ((/= "[dependencies]") . T.strip)
                . T.lines

-- | The workspace dependency graph: crate -> its in-workspace deps.
-- readGlob isolates per-file read failures (#328); a Cargo.toml that fails to
-- read (binary, permission) is dropped from the graph rather than aborting.
crateGraph :: M (Map.Map Text [Text])
crateGraph = do
  rs <- readGlob "*/Cargo.toml"
  let bodies = [ c | r <- rs, Right c <- [r.contents] ]
      g0    = Map.fromList [ (crateName b, crateDeps b) | b <- bodies, crateName b /= "" ]
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

-- | External (dep, version-spec) pairs from a Cargo.toml body. Section-aware
-- over ALL dep tables ([dependencies], [dev-], [build-], [workspace.deps]);
-- path-only and workspace=true entries carry no version and are skipped.
depSpecs :: Text -> [(Text, Text)]
depSpecs body = go False (T.lines body)
  where
    isDepHeader l = any (`T.isPrefixOf` l)
      ["[dependencies", "[dev-dependencies", "[build-dependencies", "[workspace.dependencies"]
    go _ [] = []
    go inDep (l : ls)
      | T.isPrefixOf "[" (T.strip l) = go (isDepHeader (T.strip l)) ls
      | inDep = maybe id (:) (entry l) (go inDep ls)
      | otherwise = go inDep ls
    entry l = case T.splitOn "=" (T.strip l) of
      (name : rest@(_ : _))
        | not (T.null (T.strip name)), not (T.isPrefixOf "#" (T.strip l)) ->
            let rhs = T.strip (T.intercalate "=" rest)
                nm  = T.strip (T.takeWhile (/= '.') name)
                ver | T.isPrefixOf "{" rhs = case T.splitOn "version" rhs of
                        (_ : v : _) -> quoted v
                        _           -> ""
                    | T.isInfixOf "workspace" name = ""
                    | otherwise = quoted rhs
            in if T.null ver then Nothing else Just (nm, ver)
      _ -> Nothing
    quoted t = T.takeWhile (/= '"') (T.drop 1 (T.dropWhile (/= '"') t))

-- | Version-spec skew across the workspace's Cargo.toml files, split by
-- caret-semver severity: realSkew = specs disagree on MAJOR (duplicate
-- compiled copies); cosmetic = spellings differ but unify to one version.
-- Internal tidepool-* crates are excluded (path deps, versions vestigial).
depSkew :: M Value
depSkew = do
  rs <- readGlob "**/Cargo.toml"
  let entries = [ (n, v, r.path) | r <- rs, Right c <- [r.contents]
                , (n, v) <- depSpecs c, not (T.isPrefixOf "tidepool" n) ]
      byDep   = Map.toList (Map.fromListWith (<>) [ (n, Set.singleton v) | (n, v, _) <- entries ])
      major   = T.takeWhile (/= '.')
      report sel = object
        [ (n, toJSON (L.sort (L.nub [ (v, p) | (n', v, p) <- entries, n' == n ])))
        | (n, vs) <- byDep, Set.size vs > 1, sel (Set.size (Set.map major vs) > 1) ]
  pure (object [("realSkew", report id), ("cosmetic", report not)])

-- | Self-test: depSpecs reads inline, table, and brace-form specs; skips
-- path-only, workspace=true, and non-dep sections.
depSpecsT :: Bool
depSpecsT =
  depSpecs (T.unlines
    [ "[package]", "name = \"x\"", "version = \"0.1\""
    , "[dependencies]"
    , "serde = \"1\""
    , "clap = { version = \"4.4\", features = [\"derive\"] }"
    , "tidepool-repr = { path = \"../tidepool-repr\" }"
    , "anyhow.workspace = true"
    , "[dev-dependencies]"
    , "proptest = \"1\""
    ]) == [("serde", "1"), ("clap", "4.4"), ("proptest", "1")]

-- | Self-test: topoOrder places deps before dependents on a known graph.
buildOrderT :: Bool
buildOrderT =
  let g = Map.fromList [("a", []), ("b", ["a"]), ("c", ["a", "b"])]
      o = topoOrder g
      before x y = case (L.elemIndex x o, L.elemIndex y o) of { (Just i, Just j) -> i < j; _ -> False }
  in before "a" "b" && before "b" "c" && before "a" "c" && not (any (T.isPrefixOf "CYCLE") o)
