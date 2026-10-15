{-# LANGUAGE OverloadedStrings, DataKinds #-}
module Main where
import Prelude
import Data.Proxy (Proxy (..))
import Tidepool.Aeson
import Tidepool.Aeson.Schema (JsonSchema (..))
import Tidepool.Agent.Contract
import Tidepool.Reload
import Tidepool.Effects.Core (SourceRevision (..))

assert :: Bool -> String -> IO ()
assert True _ = pure ()
assert False message = fail message
main :: IO ()
main = do
  assert (fromJSON (object []) == Success (ReloadInput [] False)) "omitted reload options changed defaults"
  assert (fromJSON (object ["also_check" .= (["Extra"] :: [String]), "accept_cache_invalidation" .= True]) == Success (ReloadInput ["Extra"] True)) "explicit policy lost"
  case (fromJSON (object ["accept_cache_invalidation" .= ("yes" :: String)]) :: Result ReloadInput) of
    Error _ -> pure ()
    Success _ -> fail "malformed policy flag accepted"
  case compileInstalledTools (specTools (defaultWorkbenchSpec :: AgentSpec (WorkbenchTools '[]) '[])) of
    Left problem -> fail (show problem)
    Right compiled -> do
      assert (map dtdName (declarations compiled) == ["haskell","haskell_sync","reload"]) "default workbench did not explicitly compose reload exactly once"
      case filter ((== "reload") . dtdName) (declarations compiled) of
        [declaration] -> assert (dtdSchedule declaration == BeforeNextInference && dtdImplementation declaration == ResidentHandler) "default reload lost ordinary precompiled Sync dispatch"
        _ -> fail "default reload declaration missing or duplicated"
  let revision = SourceRevision "issued-content" 0 []
      diff = ReloadSurfaceDiff ["added"] ["removed"] ["changed"]
      outcome = ReloadPublished revision revision [] diff
      expected = object ["Right" .= object ["tag" .= ("ReloadPublished" :: String), "contents" .= (revision,revision,([] :: [String]),diff)]]
  assert (toJSON (Right outcome :: Either ReloadError ReloadOutcome) == expected) "publication receipt must retain structured revisions and diff"
  let refusal = ReloadRefused revision revision (ReloadRefusal SurfaceIncompatible "changed tools" diff)
  case toJSON refusal of
    String _ -> fail "typed refusal collapsed to presenter prose"
    _ -> pure ()
  assert (jsonSchema (Proxy :: Proxy (Either ReloadError ReloadOutcome)) /= Null) "semantic output lacks schema"
