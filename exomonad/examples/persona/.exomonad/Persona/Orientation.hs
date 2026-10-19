{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module Persona.Orientation (renderMemory, renderRuntime, renderEffectDocs, renderCurrentEffectDocs, startup, startupWithRuntime) where

import Prelude
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Agent.Contract (StartupInput, ContextSeed, contextSection)
import Tidepool.Effects.Core (FsRead, FsWrite, RuntimeInfo, RuntimeInfoError)
import qualified Tidepool.Effects as E
import qualified Tidepool.Agent.Context as C
import Tidepool.Records (FsError)
import qualified Persona.Memory as Memory

-- This is your editable arrival context. Try adding a short current-project
-- note or choosing which material matters for the conversation you are having.
-- Context.refresh reuses renderMemory; change it when you want the same policy
-- at arrival and refresh. Reload installs code without rerunning startup.
-- Full authored bodies are carried unchanged. Reading or refreshing never
-- creates a missing memory; only startup initialises the seed files.
renderMemory :: Member FsRead effects => Eff effects (Either FsError Text)
renderMemory = do
  let blocks = [Memory.Persona, Memory.Human, Memory.Curiosities]
  observed <- traverse Memory.recall blocks
  pure $ T.intercalate "\n\n" . zipWith section blocks <$> sequence observed
  where
    section block body = "[Memory file: " <> Memory.path block <> "]\n" <> body

startup :: (Member FsRead effects, Member FsWrite effects)
  => StartupInput -> Eff effects ContextSeed
startup _ = do
  created <- Memory.initialise
  case sequence created of
    Left problem -> error (show problem)
    Right _ -> do
      observed <- renderMemory
      case observed of
        Left problem -> error (show problem)
        Right body -> pure $ contextSection "persona memory"
          (Just "Persona.Orientation.startup; .exomonad/state/memory/{persona,human,curiosities}.md") body

-- | Opt-in authored composition. Runtime observations are separate from memory
-- selection; a startup query reports its current phase, not future publication.
-- This runtime section is arrival-only: Context.refresh replaces the aggregate
-- startup block with memory. Call renderRuntime again for a fresh observation.
renderRuntime :: Member RuntimeInfo effects => Eff effects (Either RuntimeInfoError Text)
renderRuntime = fmap renderSnapshot <$> E.getRuntimeInfo
  where
    shown value = T.pack (show value)
    renderSnapshot info = T.intercalate "\n"
      [ maybe "Selected spec: unavailable" renderSpec (E.runtimeExecutingSpec info)
      , "Executing notebook generation: " <> maybe "unavailable" shown (E.runtimeExecutingGeneration info)
      , case E.runtimeConfiguredInference info of
          E.RuntimeInferenceUnavailable -> "Configured inference: unavailable"
          E.RuntimeConfiguredInference model effort -> "Configured inference: " <> shown model <> "; effort " <> shown effort
      , "Startup as of this observation: " <> renderStartup (E.runtimeStartup info)
      ]
    renderSpec spec = T.intercalate "\n"
      [ "Selected spec: " <> maybe "live value (no entry name)" id (E.runtimeSpecEntry spec)
          <> "; install " <> shown (E.runtimeSpecInstall spec)
      , "Executing source: " <> E.runtimeSpecSource spec
      , "Tools: " <> T.intercalate ", " (map renderTool (E.runtimeSpecTools spec))
      ]
    renderTool tool = E.runtimeToolName tool <> " (" <> case E.runtimeToolSchedule tool of
      E.RuntimeAsynchronous -> "asynchronous)"
      E.RuntimeBeforeNextInference -> "before next inference)"
    renderStartup E.RuntimeStartupNotAdmitted = "not admitted"
    renderStartup (E.RuntimeStartupObserved observed) = case observed of
      C.InitialContextAbsent -> "no initializer declared"
      C.InitialContextCaptured -> "captured parent context"
      C.InitialContextPreparing _ -> "preparing"
      C.InitialContextRetained _ _ -> "result retained; publication not acknowledged"
      C.InitialContextPublishedEmpty _ _ -> "empty result published"
      C.InitialContextPublished _ _ _ _ -> "context published"
      C.InitialContextFailed _ detail -> "failed: " <> detail
      C.InitialContextCancelled _ -> "cancelled"
      C.InitialContextUncertain _ detail -> "recovery required: " <> detail

-- | Pure presentation of selected catalogue facts. The typed snapshot remains
-- available to callers that want to select or compose a different context.
renderEffectDocs :: E.EffectDocsSnapshot -> Text
renderEffectDocs snapshot = T.intercalate "\n\n" $
  ("Executing effects; install " <> T.pack (show (E.effectDocsInstall snapshot))
    <> "; source " <> E.effectDocsSource snapshot
    <> "\nEffect membership does not establish resource grants or credential availability.")
  : map renderEffect (E.effectDocsEffects snapshot)
  where
    renderEffect effect = T.pack (show (E.effectDocumentationIdentity effect)) <> "\n"
      <> case E.effectDocumentationBody effect of
        E.EffectDocumentationMissing -> "Catalogue documentation unavailable."
        E.EffectDocumentationAvailable facts -> T.intercalate "\n" $
          E.effectDocumentationDescription facts
          : E.effectDocumentationImports facts
          <> map renderOperation (E.effectDocumentationOperations facts)
    renderOperation operation = T.intercalate "\n" $
      E.effectOperationSignature operation
      : E.effectOperationContract operation
      <> E.effectOperationExamples operation
      <> maybe [] (\source -> ["Source: " <> source]) (E.effectOperationSource operation)

renderCurrentEffectDocs :: Member RuntimeInfo effects => Eff effects (Either RuntimeInfoError Text)
renderCurrentEffectDocs = fmap renderEffectDocs <$> E.currentEffectDocs

startupWithRuntime :: (Member FsRead effects, Member FsWrite effects, Member RuntimeInfo effects)
  => StartupInput -> Eff effects ContextSeed
startupWithRuntime input = do
  memory <- startup input
  observed <- renderRuntime
  case observed of
    Left problem -> error (show problem)
    Right body -> do
      docs <- renderCurrentEffectDocs
      case docs of
        Left problem -> error (show problem)
        Right documentation -> pure $ memory
          <> contextSection "runtime at arrival"
            (Just "getRuntimeInfo; observed during initialization") body
          <> contextSection "effects at arrival"
            (Just "currentEffectDocs; exact executing installation catalogue") documentation
