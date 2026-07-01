{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, OverloadedRecordDot #-}
-- | Dogfood: Rust panic-site audit. Deterministic census now; the
-- ration-attention judgment layer (safe-by-invariant vs risky) comes later.
-- Core lives here; eval is a thin shell (`panicReport`).
module RustAudit where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import qualified Data.Text as T

-- A panic-ish site: file, 1-based line, kind, raw line text.
data Site = Site { sFile :: Text, sLine :: Int, sKind :: Text, sText :: Text }

-- Most-specific kind for a matched line, or Nothing if it's a false hit.
kindOf :: Text -> Maybe Text
kindOf t
  | isInfixOf "unreachable!"   t = Just "unreachable"
  | isInfixOf "unimplemented!" t = Just "unimplemented"
  | isInfixOf "todo!"          t = Just "todo"
  | isInfixOf "panic!"         t = Just "panic"
  | isInfixOf ".expect("       t = Just "expect"
  | isInfixOf ".unwrap()"      t = Just "unwrap"
  | otherwise                    = Nothing

crateOf :: Text -> Text
crateOf f = case splitOn "/" f of
  (c : _) -> c
  []      -> "?"

isTestSite :: Site -> Bool
isTestSite s =
  isInfixOf "/tests/" (sFile s)
    || isInfixOf "#[test]" (sText s)
    || isInfixOf "#[cfg(test" (sText s)

tally :: [Text] -> [(Text, Int)]
tally = foldl' bump []
  where
    bump acc k = ins k acc
    ins k [] = [(k, 1)]
    ins k ((k', n) : rest) = if k == k' then (k', n + 1) : rest else (k', n) : ins k rest

rankDesc :: [(Text, Int)] -> [(Text, Int)]
rankDesc = sortBy (\a b -> compare (snd b) (snd a))

panicSites :: M [Site]
panicSites = do
  hits <- grepGlob "\\.unwrap\\(\\)|\\.expect\\(|panic!|unreachable!|unimplemented!|todo!" "**/*.rs"
  pure [ Site h.path h.line k h.text | h <- hits, Just k <- [kindOf h.text] ]

-- numbered context window around a 1-based line.
contextAround :: Int -> Int -> Text -> Text
contextAround target radius content =
  let keep (i, _) = i + 1 >= target - radius && i + 1 <= target + radius
      num (i, l) = pack (show (i + 1)) <> "| " <> l
  in unlines (map num (filter keep (zipWithIndex (lines content))))

-- The ration-attention layer: deterministic site-finding, LLM judgment per site.
auditFile :: Text -> M Value
auditFile file = do
  content <- readFile file
  let sites = [ (i + 1, k) | (i, l) <- zipWithIndex (lines content), Just k <- [kindOf l] ]
      block (ln, k) = "### line " <> pack (show ln) <> " (" <> k <> ")\n" <> contextAround ln 3 content
      payload = intercalate "\n\n" (map block sites)
      prompt = "Triage Rust panic sites in " <> file
            <> ". For EACH site rate panic risk under adversarial/unexpected input: "
            <> "low = infallible or invariant-guaranteed; medium = depends on caller contract; "
            <> "high = reachable on bad/external/malformed input. One entry per site, cite its line.\n\n"
            <> payload
  llm (SArr (SObj [ ("line", SNum), ("kind", SStr)
                  , ("risk", SEnum ["low", "medium", "high"]), ("reason", SStr) ])) prompt

-- Cheap, line-local risk bucket. The point: shrink the LLM-judged residue to a
-- small ABSOLUTE number. If "unknown" stays huge, line-local heuristics aren't
-- enough and the tool isn't economical without context-reading.
bucket :: Site -> Text
bucket s =
  let t = sText s
      has p = isInfixOf p t
  in if sKind s == "expect"                                       then "expect-intentional"
     else if has ".lock()" || has ".read()" || has ".write()"    then "lock-poison"
     else if has "write!" || has "writeln!" || has "format!"     then "infallible-write"
     else if has "Regex::new" || has "regex!"                    then "regex-literal"
     else if has "parse()" || has "from_utf8" || has "from_str"
          || has "try_into" || has "try_from"                    then "parse-fallible"
     else if has ".get(" || has ".first(" || has ".last("
          || has ".pop(" || has ".next("                         then "option-access"
     else "unknown"

needsJudgment :: Text -> Bool
needsJudgment b = b == "parse-fallible" || b == "option-access" || b == "unknown"

triageReport :: M Value
triageReport = do
  sites <- panicSites
  let prod  = filter (not . isTestSite) sites
      needs = filter (needsJudgment . bucket) prod
  pure (object
    [ "prod"      .= len prod
    , "buckets"   .= rankDesc (tally (map bucket prod))
    , "needs_llm" .= len needs
    , "sample"    .= take 12 (map (\s -> object
                        ["f" .= sFile s, "l" .= sLine s, "b" .= bucket s, "t" .= sText s]) needs)
    ])

-- Iteration 2: cluster the ambiguous residue by the fallible OP just before the
-- unwrap/expect (next/get/lock/pop/parse/...). One judgment per op-cluster, not
-- per site. Measures whether clustering collapses the residue to something cheap.
opKey :: Site -> Text
opKey s =
  let line   = sText s
      marker = if isInfixOf ".expect" line then ".expect" else ".unwrap"
      prefix = fst (T.breakOn marker line)
      seg    = case splitOn "." prefix of { [] -> ""; xs -> last xs }
      ident  = T.takeWhile isIdentChar seg
  in if ident == "" then "<expr>" else ident
  where
    isIdentChar c = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')
                 || (c >= '0' && c <= '9') || c == '_'

clusterReport :: M Value
clusterReport = do
  sites <- panicSites
  let prod     = filter (not . isTestSite) sites
      unknowns = filter ((== "unknown") . bucket) prod
      byOp     = rankDesc (tally (map opKey unknowns))
  pure (object
    [ "unknowns"         .= len unknowns
    , "distinct_ops"     .= len byOp
    , "covered_by_top10" .= sum (map snd (take 10 byOp))
    , "top_ops"          .= take 20 byOp
    ])

panicReport :: M Value
panicReport = do
  sites <- panicSites
  let prod = filter (not . isTestSite) sites
  pure (object
    [ "total"    .= len sites
    , "prod"     .= len prod
    , "by_kind"  .= rankDesc (tally (map sKind prod))
    , "by_crate" .= rankDesc (tally (map (crateOf . sFile) prod))
    , "hotspots" .= take 15 (rankDesc (tally (map sFile prod)))
    ])
