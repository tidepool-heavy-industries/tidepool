{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, OverloadedRecordDot #-}
-- | Mech v2: a rubric-filling crate-walk — gated, evidence-fed, checkpointed.
--
--   GATE (encoded judgment): surface only the top-3 production-loudest crates.
--   DASHBOARD: real sample panic lines, so the pilot judges from EVIDENCE, not
--     from knowing what the crate is (the gap v1 exposed).
--   RUBRIC: a structured form the pilot fills (input-handling vs invariant + a
--     recommendation) — the crystallized judgment a weak pilot just completes.
--   CHECKPOINT: each verdict is persisted to KV the instant it lands, and a
--     re-entry skips decided crates — so a lost continuation costs ONE junction,
--     not the whole walk. Durability of the continuation stops mattering.
module MechDemo where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import qualified Data.Text as T

data Dash = Dash { dCrate :: Text, dProd :: Int, dSample :: [Text] }

crateDir :: Text -> Text
crateDir p = case splitOn "/" p of { (c : _) -> c; [] -> p }

isTest :: Text -> Text -> Bool
isTest f t = isInfixOf "/tests/" f || isInfixOf "#[test]" t || isInfixOf "#[cfg(test" t

-- deterministic drivetrain: the test-filtered dashboard + a sample of evidence.
crateDash :: Text -> M Dash
crateDash crate = do
  hits <- grepGlob "\\.unwrap\\(\\)|panic!|\\.expect\\(" (crate <> "/**/*.rs")
  let prod = [ h | h <- hits, not (isTest h.path h.text) ]
      fmt h = h.path <> ":" <> pack (show h.line) <> "  " <> T.strip h.text
  pure (Dash crate (len prod) (take 6 (map fmt prod)))

-- the cockpit: pause, hand over evidence + a rubric, checkpoint the answer.
rubricJunction :: Dash -> M Value
rubricJunction d = do
  let key = "mech:panic:" <> dCrate d
  cached <- kvGet key
  case cached of
    Just v -> pure (object ["crate" .= dCrate d, "verdict" .= v, "from_checkpoint" .= True])
    Nothing -> do
      let prompt = "Crate '" <> dCrate d <> "' — " <> pack (show (dProd d))
                <> " production panic sites. Sample:\n"
                <> intercalate "\n" (dSample d)
                <> "\n\nFrom these sample lines alone, classify the dominant kind and recommend."
      ans <- ask (SObj [ ("dominant_kind",  SEnum ["input-handling", "invariant-assert", "mixed"])
                       , ("recommendation", SEnum ["harden", "leave", "focus"])
                       , ("rationale",      SStr) ]) prompt
      kvSet key ans
      pure (object ["crate" .= dCrate d, "verdict" .= ans, "from_checkpoint" .= False])

crateWalk :: M Value
crateWalk = do
  cargos <- glob "*/Cargo.toml"
  dashes <- mapM (crateDash . crateDir) cargos
  let loud = take 3 (sortBy (\a b -> compare (dProd b) (dProd a)) dashes)
  rubrics <- mapM rubricJunction loud
  pure (object ["considered" .= len dashes, "surfaced" .= map dCrate loud, "rubrics" .= rubrics])
