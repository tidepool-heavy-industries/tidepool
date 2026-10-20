{-# LANGUAGE OverloadedStrings, DataKinds #-}
module Main where
import Prelude
import Data.Proxy (Proxy (..))
import qualified Data.Text as T
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
  assert (fromJSON (object []) == Success (ReloadInput [] False False)) "omitted reload options changed defaults"
  mapM_ (\(accept, stopServices) ->
    assert (fromJSON (object ["also_check" .= (["Extra"] :: [String]), "accept_cache_invalidation" .= accept, "force" .= stopServices]) == Success (ReloadInput ["Extra"] accept stopServices)) "force and surface policy were conflated")
    [(False, False), (False, True), (True, False), (True, True)]
  case (fromJSON (object ["accept_cache_invalidation" .= ("yes" :: String)]) :: Result ReloadInput) of
    Error _ -> pure ()
    Success _ -> fail "malformed policy flag accepted"
  case (fromJSON (object ["force" .= ("yes" :: String)]) :: Result ReloadInput) of
    Error _ -> pure ()
    Success _ -> fail "malformed force flag accepted"
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

  let operation = ReloadOperation "exec-issued" 2 3
      actor = ReloadActorResource (42, 7)
      nextActor = ReloadActorResource (42, 8)
      cause = ReloadCause ReloadAcknowledgementUnconfirmed "cleanup owner retained"
      stopped = ReloadResourceOutcome actor ReloadStopConfirmed ReloadCleanupConfirmed
      refused = ReloadResourceOutcome nextActor
        (ReloadStopRefused (ReloadCause ReloadStopFailed "still live")) ReloadCleanupNotRequired
      pending = ReloadResourceOutcome nextActor ReloadStopPending ReloadCleanupPending
      unknown = ReloadResourceOutcome (ReloadActorResource (42, 9))
        (ReloadStopUnconfirmed cause) (ReloadCleanupUnknown cause)
      blockers =
        [ ReloadResourceBlocker actor ReloadStartingResource
        , ReloadResourceBlocker (ReloadDisplayResource operation) ReloadPendingProducer
        , ReloadResourceBlocker (ReloadDisplayResource operation) ReloadDisplayAcknowledgementPending
        , ReloadResourceBlocker (ReloadDisplayResource operation)
            (ReloadDisplayAcknowledgementUnconfirmed "packet acknowledgement lost")
        , ReloadCollateralBlocker actor nextActor ReloadPhysicalSupervision
        ]
      receipt state effects = ReloadRetirement (ReloadRetirementReceipt
        (SourceRevision "frozen-A" 0 []) (SourceRevision "frozen-B" 0 [])
        (Just operation) state blockers effects diff (Just (primaryFor state)) (secondaryFor state))
      primaryFor state = case state of
        ReloadAReopened -> ReloadCause ReloadRetirementBlocked "known live blocker"
        ReloadRetirementFenced _ -> cause
        ReloadBPublished _ _ -> ReloadCause ReloadContinuationFailed "receipt display failed"
      secondaryFor state = case state of
        ReloadBPublished _ _ -> [ReloadCause ReloadContinuationFailed "response delivery failed"]
        _ -> [ReloadCause ReloadPublicationFailed "publication uncommitted"]
      render state effects = reloadPresenter (Right (receipt state effects))
      reopened = render ReloadAReopened [stopped, refused]
      fenced = render (ReloadRetirementFenced ReloadResourceCleanup) [stopped, pending, unknown]
      published = render (ReloadBPublished ["Changed"] ["repair projection"]) [stopped]
  mapM_ (\fragment -> assert (fragment `T.isInfixOf` reopened)
    ("partial stop receipt lost " ++ T.unpack fragment))
    [ "frozen-A reopened", "frozen-B was not published", "actor 42/7: stop confirmed, cleanup confirmed"
    , "actor 42/8: stop refused", "still live", "stopped resources remain stopped"
    , "exec-issued/unit=2/effect=3", "producer pending", "display acknowledgement pending"
    , "display acknowledgement unconfirmed", "also affects actor 42/8 via physical supervision"
    ]
  mapM_ (\fragment -> assert (fragment `T.isInfixOf` fenced)
    ("fenced receipt lost " ++ T.unpack fragment))
    [ "remains fenced at physical cleanup", "Primary: acknowledgement unconfirmed"
    , "Secondary causes: publication failed", "stop pending, cleanup pending"
    , "stop unconfirmed", "cleanup unconfirmed", "reconcile before retry"
    ]
  assert (not ("reopened" `T.isInfixOf` fenced)) "uncertain cleanup presented as reopened"
  assert ("Published source frozen-B" `T.isInfixOf` published) "confirmed candidate lost"
  assert ("Notebook reset" `T.isInfixOf` published) "published reset contract lost"
  assert ("Warnings: repair projection" `T.isInfixOf` published) "published warnings lost"
  assert ("Primary: retirement blocked" `T.isInfixOf` reopened) "reopened receipt lost primary cause"
  assert ("Secondary causes: continuation failed" `T.isInfixOf` published) "published receipt lost secondary cause"
  assert (toJSON (receipt ReloadAReopened []) /=
    toJSON (receipt (ReloadRetirementFenced ReloadResourceStop) [])) "reopened and fenced codec collapsed"
  assert (toJSON pending /= toJSON unknown) "pending and unconfirmed codec collapsed"
  assert (toJSON actor /= toJSON nextActor) "actor incarnations collapsed"
  assert (toJSON (ReloadWorkbenchReservation "exec-issued" "attempt-a") /=
    toJSON (ReloadWorkbenchReservation "exec-issued" "attempt-b")) "reservation attempts collapsed"
  assert (toJSON (ReloadNativeLocal "18446744073709551615") /=
    toJSON (ReloadNativeRemote "0" "18446744073709551615")) "native identity tag or unsigned coordinates lost"
  let maximumUnsigned = "18446744073709551615"
      actorCompiler = ReloadActorCompilerResource (42, 7) maximumUnsigned
      preparationCompiler = ReloadPreparationCompilerResource maximumUnsigned maximumUnsigned
      invocationCompiler = ReloadCompilerResource (42, 7) (ReloadWorkbenchReservation "exec-issued" "attempt-a") 0
      compilerResources = [actorCompiler, preparationCompiler, invocationCompiler]
      sourceBinding = ReloadSourceBindingResource (42, 7) maximumUnsigned
  mapM_ (\resource -> assert (fromJSON (toJSON resource) == Success resource)
    "compiler resource observation did not roundtrip") compilerResources
  assert (toJSON actorCompiler == object
    ["tag" .= ("ReloadActorCompilerResource" :: String), "contents" .= ((42, 7) :: (Int, Int), maximumUnsigned :: T.Text)])
    "actor compiler resource lost actor or unsigned registration coordinate"
  assert (toJSON preparationCompiler == object
    ["tag" .= ("ReloadPreparationCompilerResource" :: String), "contents" .= (maximumUnsigned :: T.Text, maximumUnsigned :: T.Text)])
    "preparation compiler resource lost unsigned coordinates"
  assert (toJSON actorCompiler /= toJSON preparationCompiler &&
    toJSON actorCompiler /= toJSON invocationCompiler &&
    toJSON preparationCompiler /= toJSON invocationCompiler)
    "compiler resource owners collapsed"
  assert (fromJSON (toJSON sourceBinding) == Success sourceBinding)
    "source binding observation did not roundtrip"
  assert (toJSON sourceBinding == object
    ["tag" .= ("ReloadSourceBindingResource" :: String), "contents" .= ((42, 7) :: (Int, Int), maximumUnsigned :: T.Text)])
    "source binding lost its actual actor or unsigned stable slot"
  assert (toJSON sourceBinding /= toJSON (ReloadWatchResource (42, 7) 0))
    "source binding was replaced by a watch coordinate"
  let compilerReceipt = render ReloadAReopened
        [ReloadResourceOutcome resource ReloadStopPending ReloadCleanupPending | resource <- sourceBinding : compilerResources]
  mapM_ (\fragment -> assert (fragment `T.isInfixOf` compilerReceipt)
    "compiler resource presenter lost exact owner or coordinate")
    [ "actor 42/7 compiler registration=18446744073709551615"
    , "preparation 18446744073709551615 compiler registration=18446744073709551615"
    , "actor 42/7 compiler exec-issued/attempt=attempt-a/registration=0"
    , "actor 42/7 source binding slot=18446744073709551615"
    ]
  assert (jsonSchema (Proxy :: Proxy ReloadRetirementReceipt) /= Null) "retirement receipt lacks generated schema"

  -- Cover every tuple arity supported by the actual JSON codecs. The missing
  -- pair instance originally prevented generated ReloadResource from compiling.
  let fixed size fields = object
        [ "type" .= ("array" :: String), "prefixItems" .= fields
        , "minItems" .= (size :: Int), "maxItems" .= size ]
      intSchema = object ["type" .= ("integer" :: String)]
      textSchema = object ["type" .= ("string" :: String)]
      boolSchema = object ["type" .= ("boolean" :: String)]
      schemas =
        [ (jsonSchema (Proxy :: Proxy (Int, String)), fixed 2 [intSchema, textSchema])
        , (jsonSchema (Proxy :: Proxy (Int, String, Bool)), fixed 3 [intSchema, textSchema, boolSchema])
        , (jsonSchema (Proxy :: Proxy (Int, String, Bool, Int)), fixed 4 [intSchema, textSchema, boolSchema, intSchema])
        , (jsonSchema (Proxy :: Proxy (Int, String, Bool, Int, String)), fixed 5 [intSchema, textSchema, boolSchema, intSchema, textSchema])
        ]
  mapM_ (\(actual, expectedSchema) -> assert (actual == expectedSchema) "tuple schema lost fixed arity or element order") schemas
  assert (toJSON ((42, 7) :: (Int, Int)) == toJSON ([42, 7] :: [Int])) "actor address tuple encoder changed"
