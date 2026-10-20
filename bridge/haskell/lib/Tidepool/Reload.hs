{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeOperators #-}
{-# OPTIONS_GHC -Wno-simplifiable-class-constraints -Wno-orphans #-}
-- | Typed source publication through an admitted synchronous invocation.
module Tidepool.Reload
  ( ReloadInput (..), ReloadError (..), ReloadOutcome (..)
  , ReloadOptions (..), ReloadPolicy (..), ReloadSurfaceDiff (..)
  , ReloadRefusal (..), ReloadRefusalCategory (..)
  , ReloadOperation (..), ReloadReservation (..), ReloadNativeIdentity (..), ReloadResource (..)
  , ReloadBlockerReason (..), ReloadLifetimeEdge (..), ReloadBlocker (..)
  , ReloadCauseKind (..), ReloadCause (..), ReloadStopOutcome (..)
  , ReloadCleanupOutcome (..), ReloadResourceOutcome (..)
  , ReloadRetirementStage (..), ReloadRetirementState (..), ReloadRetirementReceipt (..)
  , reloadSource, reloadSourceWith, reloadHandler, reloadPresenter
  ) where
import Control.Monad.Freer (Eff, Member)
import Data.Proxy (Proxy (..))
import Data.Text (Text)
import GHC.Generics (Generic)
import qualified Tidepool.Data.Text as T
import Tidepool.Aeson.FromJSON (FromJSON (..), withObject, (.:?), (.!=))
import Tidepool.Aeson.Value (Value (..), ToJSON (..), object, (.=))
import Tidepool.Aeson.Schema (JsonSchema (..), objectSchema)
import Tidepool.Effects.Core
  ( Reload, ReloadError (..), ReloadOutcome (..), ReloadOptions (..)
  , ReloadPolicy (..), ReloadSurfaceDiff (..), ReloadRefusal (..), ReloadRefusalCategory (..)
  , SourceRevision (..), reloadSourceWith
  , ReloadOperation (..), ReloadReservation (..), ReloadNativeIdentity (..), ReloadResource (..)
  , ReloadBlockerReason (..), ReloadLifetimeEdge (..), ReloadBlocker (..)
  , ReloadCauseKind (..), ReloadCause (..), ReloadStopOutcome (..)
  , ReloadCleanupOutcome (..), ReloadResourceOutcome (..)
  , ReloadRetirementStage (..), ReloadRetirementState (..), ReloadRetirementReceipt (..)
  )

-- | Optional hosted reload arguments. Surface consent and service retirement
-- are independent decisions; omitted values grant neither.
data ReloadInput = ReloadInput
  { also_check :: [Text]
  , accept_cache_invalidation :: Bool
  , force :: Bool
  } deriving (Show, Eq, Generic)

instance FromJSON ReloadInput where
  parseJSON = withObject "ReloadInput" $ \o -> ReloadInput
    <$> ((o .:? "also_check") .!= [])
    <*> ((o .:? "accept_cache_invalidation") .!= False)
    <*> ((o .:? "force") .!= False)
instance ToJSON ReloadInput
instance JsonSchema ReloadInput where
  jsonSchema _ = object
    [ "type" .= ("object" :: Text)
    , "properties" .= object ["also_check" .= jsonSchema (Proxy :: Proxy [Text]), "accept_cache_invalidation" .= jsonSchema (Proxy :: Proxy Bool), "force" .= jsonSchema (Proxy :: Proxy Bool)]
    , "additionalProperties" .= False
    ]

-- The generated Core owns ReloadError; this companion supplies its authored schema.
instance JsonSchema ReloadError where
  jsonSchema _ = object [("oneOf", Array
    [ objectSchema Nothing [("tag", object ["enum" .= (["ReloadUnavailable"] :: [Text])], True), ("detail", jsonSchema (Proxy :: Proxy Text), True)]
    , objectSchema Nothing [("tag", object ["enum" .= (["ReloadUnreadable"] :: [Text])], True), ("detail", jsonSchema (Proxy :: Proxy Text), True)]
    ])]

-- | Compatible publication. Changed source may still change the prompt prefix.
reloadSource :: Member Reload effects => [Text] -> Eff effects (Either ReloadError ReloadOutcome)
reloadSource checks = reloadSourceWith (ReloadOptions checks CompatibleOnly False)

reloadHandler :: Member Reload effects => ReloadInput -> Eff effects (Either ReloadError ReloadOutcome)
reloadHandler (ReloadInput checks accept stopServices) = reloadSourceWith
  (ReloadOptions checks (if accept then AcceptCacheInvalidation else CompatibleOnly) stopServices)

reloadPresenter :: Either ReloadError ReloadOutcome -> Text
reloadPresenter result = case result of
  Left (ReloadUnavailable detail) -> "Reload unavailable: " <> detail
  Left (ReloadUnreadable detail) -> "Reload unreadable: " <> detail
  Right outcome -> case outcome of
    ReloadRetirement receipt -> retirementSummary receipt
    ReloadUnchanged _ -> "Configuration unchanged; notebook bindings preserved."
    ReloadPublished _ (SourceRevision identity _ _) _ diff -> "Published source " <> identity <> surfaceSummary diff <> resetReminder
    ReloadRejected _ _ diagnostics -> "Preparation rejected: " <> diagnostics
    ReloadRefused _ _ (ReloadRefusal category detail diff) -> "Reload refused (" <> refusalCategorySummary category <> "): " <> detail <> surfaceSummary diff
    ReloadCancelled _ -> "Reload cancelled before publication."
    ReloadPublishedWithWarnings _ (SourceRevision identity _ _) _ warnings diff ->
      "Published source " <> identity <> surfaceSummary diff <> resetReminder <> " Warnings: " <> T.pack (show warnings)
    ReloadPublicationUnconfirmed _ detail -> "Publication unconfirmed; reconcile before retry: " <> detail
  where
    resetReminder = " Notebook reset; re-import modules and recreate notebook bindings before use."
    surfaceSummary (ReloadSurfaceDiff added removed changed) =
      " (tools added " <> count added <> ", removed " <> count removed <> ", changed " <> count changed <> ")."
    count = T.pack . show . length

refusalCategorySummary :: ReloadRefusalCategory -> Text
refusalCategorySummary category = case category of
  PreparationFailed -> "preparation failed"
  SurfaceIncompatible -> "surface incompatible"
  UnsupportedSurface -> "unsupported surface"
  AuthorityDenied -> "authority denied"

retirementSummary :: ReloadRetirementReceipt -> Text
retirementSummary (ReloadRetirementReceipt previous candidate operation state blockers outcomes diff primary secondary) =
  stateSummary <> maybe "" (\op -> " Operation " <> operationSummary op <> ".") operation
    <> sections "Blockers" (map blockerSummary blockers)
    <> sections "Resource outcomes" (map outcomeSummary outcomes)
    <> maybe "" (\cause -> " Primary: " <> causeSummary cause <> ".") primary
    <> sections "Secondary causes" (map causeSummary secondary)
  where
    revision (SourceRevision identity _ _) = identity
    stateSummary = case state of
      ReloadAReopened -> "Source " <> revision previous <> " reopened; candidate " <> revision candidate
        <> " was not published. All initiated stops and cleanup settled; stopped resources remain stopped."
      ReloadRetirementFenced stage ->
        "Reload remains fenced at " <> stageSummary stage <> "; reconcile before retry."
      ReloadBPublished _ warnings -> "Published source " <> revision candidate <> surfaceSummary diff
        <> " Notebook reset; re-import modules and recreate notebook bindings before use."
        <> sections "Warnings" warnings
    surfaceSummary (ReloadSurfaceDiff added removed changed) =
      " (tools added " <> number (length added) <> ", removed " <> number (length removed)
        <> ", changed " <> number (length changed) <> ")."

sections :: Text -> [Text] -> Text
sections _ [] = ""
sections label entries = " " <> label <> ": " <> T.intercalate "; " entries <> "."

number :: Show a => a -> Text
number = T.pack . show

operationSummary :: ReloadOperation -> Text
operationSummary (ReloadOperation execution unit ordinal) =
  execution <> "/unit=" <> number unit <> "/effect=" <> number ordinal

resourceSummary :: ReloadResource -> Text
resourceSummary resource = case resource of
  ReloadActorResource address -> actor address
  ReloadNativeResource address identity -> "native " <> nativeSummary identity <> " issued to " <> actor address
  ReloadPreparationResource identity -> "preparation " <> number identity
  ReloadInvocationResource address reservation -> actor address <> " invocation " <> reservationSummary reservation
  ReloadCompilerResource address reservation ordinal -> actor address <> " compiler "
    <> reservationSummary reservation <> "/registration=" <> number ordinal
  ReloadFormResource address reservation mount -> actor address <> " form " <> mount <> " " <> reservationSummary reservation
  ReloadDisplayResource operation -> "display " <> operationSummary operation
  ReloadWatchResource address identity -> actor address <> " watch " <> number identity
  ReloadUnclassifiedPhysicalResource identity -> "unclassified physical resource " <> nativeSummary identity
  where
    actor (identity, incarnation) = "actor " <> number identity <> "/" <> number incarnation

nativeSummary :: ReloadNativeIdentity -> Text
nativeSummary identity = case identity of
  ReloadNativeLocal pid -> "local/" <> pid
  ReloadNativeRemote node pid -> "remote/" <> node <> "/" <> pid

reservationSummary :: ReloadReservation -> Text
reservationSummary reservation = case reservation of
  ReloadWorkbenchReservation execution attempt -> execution <> "/attempt=" <> attempt
  ReloadRouteReservation watch -> "route=" <> number watch
  ReloadScopeReservation scope -> "scope=" <> number scope

blockerSummary :: ReloadBlocker -> Text
blockerSummary blocker = case blocker of
  ReloadResourceBlocker resource reason -> resourceSummary resource <> " (" <> reasonSummary reason <> ")"
  ReloadCollateralBlocker stopping affected edge -> "stopping " <> resourceSummary stopping
    <> " also affects " <> resourceSummary affected <> " via " <> edgeSummary edge
  ReloadUnclassifiedSourceWork count -> number count <> " unclassified source work reservations"

reasonSummary :: ReloadBlockerReason -> Text
reasonSummary reason = case reason of
  ReloadActiveDependentWork -> "active dependent work"
  ReloadUnclassifiedWork -> "unclassified source dependence"
  ReloadStartingResource -> "starting"
  ReloadRunningResource -> "running"
  ReloadTransferringResource -> "ownership transferring"
  ReloadCleanupUnsupported -> "cleanup unsupported"
  ReloadCleanupUnconfirmed detail -> "cleanup unconfirmed: " <> detail
  ReloadCompilerPending -> "compiler END pending"
  ReloadCompilerUnconfirmed detail -> "compiler END unconfirmed: " <> detail
  ReloadPendingProducer -> "producer pending"
  ReloadDisplayAcknowledgementPending -> "display acknowledgement pending"
  ReloadDisplayAcknowledgementUnconfirmed detail -> "display acknowledgement unconfirmed: " <> detail

edgeSummary :: ReloadLifetimeEdge -> Text
edgeSummary edge = case edge of
  ReloadPhysicalSupervision -> "physical supervision"
  ReloadPendingSupervision -> "pending supervision"
  ReloadPendingResourcePreparation -> "pending resource preparation"
  ReloadResourceOwnership -> "resource ownership"
  ReloadOwnershipTransfer -> "ownership transfer"
  ReloadOwnershipTransferUnconfirmed -> "unconfirmed ownership transfer"

outcomeSummary :: ReloadResourceOutcome -> Text
outcomeSummary (ReloadResourceOutcome resource stop cleanup) = resourceSummary resource
  <> ": " <> stopSummary stop <> ", " <> cleanupSummary cleanup
  where
    stopSummary outcome = case outcome of
      ReloadStopNotRequested -> "stop not requested"
      ReloadStopPending -> "stop pending"
      ReloadStopConfirmed -> "stop confirmed"
      ReloadStopRefused cause -> "stop refused: " <> causeSummary cause
      ReloadStopUnconfirmed cause -> "stop unconfirmed: " <> causeSummary cause
    cleanupSummary outcome = case outcome of
      ReloadCleanupNotRequired -> "cleanup not required"
      ReloadCleanupPending -> "cleanup pending"
      ReloadCleanupConfirmed -> "cleanup confirmed"
      ReloadCleanupRefused cause -> "cleanup refused: " <> causeSummary cause
      ReloadCleanupUnknown cause -> "cleanup unconfirmed: " <> causeSummary cause

stageSummary :: ReloadRetirementStage -> Text
stageSummary stage = case stage of
  ReloadCandidateSettlement -> "candidate settlement"
  ReloadOriginalReopen -> "original source reopening"
  ReloadResourceStop -> "resource stop"
  ReloadResourceCleanup -> "physical cleanup"
  ReloadSourcePublication -> "source publication"

causeSummary :: ReloadCause -> Text
causeSummary (ReloadCause kind detail) = kindSummary <> ": " <> detail
  where
    kindSummary = case kind of
      ReloadRetirementBlocked -> "retirement blocked"
      ReloadRetirementCancelled -> "cancelled"
      ReloadCandidateSettlementFailed -> "candidate settlement failed"
      ReloadOriginalReopenFailed -> "original source reopening failed"
      ReloadStopFailed -> "stop failed"
      ReloadCleanupFailed -> "cleanup failed"
      ReloadPublicationFailed -> "publication failed"
      ReloadContinuationFailed -> "continuation failed"
      ReloadAcknowledgementUnconfirmed -> "acknowledgement unconfirmed"
