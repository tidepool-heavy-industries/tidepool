{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
module PersonaNotebookTest (personaNotebookTests) where

import Prelude hiding (readFile)
import Control.Exception (SomeException, evaluate, try)
import Control.Monad.Freer (Eff, Member, interpret, run)
import Control.Monad.Freer.State (State, get, modify, runState)
import qualified Data.Map.Strict as Map
import Data.Text (Text)
import qualified Data.Text as T
import Test.Tasty.HUnit (assertBool, assertEqual)
import Tidepool.Test.Runner (TestTree, testGroup, testCase)
import Tidepool.Aeson (object, (.=))
import Tidepool.Agent.Contract (StartupInput(..), renderContextSeed)
import Tidepool.Effects.Core
  (Actor, FsRead(..), FsWrite(..), Jev(..), JevCallError(..), ContextReadWrite(..))
import Tidepool.Records (FsError(..))
import qualified Tidepool.Actor.Record as R
import qualified Tidepool.Agent.Context as C
import Tidepool.Form.Wire (prepareForm, decodeSubmission)
import qualified Persona.Attention as A
import qualified Persona.AttentionNotebook as N
import qualified Persona.Orientation as O
import qualified Persona.Context as Context

collection :: A.Collection
collection = either (error . show) id $ A.collection
  [A.Memory "memory" "notes/memory.md" "An observation worth returning to." [] [] Nothing]

noJev :: Jev a -> Eff '[] a
noJev (JevAskWith _) = error "unexpected semantic call"

countJev :: Jev a -> Eff '[State Int] a
countJev (JevAskWith _) = modify @Int (+1) >> pure (Left (JevCircuitOpen 503 0))

runNotebook :: [N.Instruction] -> (N.Snapshot, Int)
runNotebook instructions =
  let ((observed, _), calls) = run $ runState (0 :: Int) $ interpret countJev $
        runState (N.notebook collection O.arrival) (N.execute instructions)
  in (observed, calls)

-- This compiles the actual record/start/client/finish combination. Execution of
-- the actor runtime belongs to joined hosted qualification, not these pure tests.
actorExample :: Member Actor effects => Eff effects N.Snapshot
actorExample = do
  actor <- R.start (N.definition mempty (N.notebook collection O.arrival))
  observed <- R.call (N.run (R.client actor)) [N.Budget (A.Limits 0 0), N.Prepare]
  _ <- R.finish actor
  pure observed

type Files = Map.Map Text Text
readHost :: FsRead a -> Eff '[FsWrite, Jev, State Files] a
readHost (FsRead file) = maybe (Left (FsNotFound file)) Right . Map.lookup file <$> get
readHost _ = error "unexpected file observation"
writeHost :: FsWrite a -> Eff '[Jev, State Files] a
writeHost (FsWrite file body) = modify (Map.insert file body) >> pure (Right ())
writeHost (FsWriteCas file _ body) = do
  files <- get @Files
  if Map.member file files then pure (Right (Left (Just "existing")))
  else modify (Map.insert file body) >> pure (Right (Right ()))
startupJev :: Jev a -> Eff '[State Files] a
startupJev (JevAskWith _) = pure (Left (JevCircuitOpen 503 0))

writeRefusingInspection :: FsWrite a -> Eff '[Jev, State Files] a
writeRefusingInspection (FsWrite "notes/initial-attention.md" _) =
  pure (Left (FsSandbox "inspection write refused"))
writeRefusingInspection other = writeHost other

personaNotebookTests :: TestTree
personaNotebookTests = testGroup "persona executable seeds"
  [ testCase "draft edits and inspection do not invoke Jev" $ do
      let (observed, calls) = runNotebook
            [N.Occasion "Follow a question", N.Budget (A.Limits 2 20000), N.View A.connections]
      assertEqual "no inference for edits" 0 calls
      assertEqual "current draft" (A.connections collection) (N.draft observed)
      assertEqual "no invented result" Nothing (N.previous observed)
      assertBool "draft awaits preparation" (N.changedSincePreparation observed)
      actorExample @'[Actor] `seq` pure ()
  , testCase "several edits precede one semantic batch and retain failure evidence" $ do
      let (observed, calls) = runNotebook
            [N.Occasion "First thought", N.Occasion "A situated question", N.Prepare]
      assertEqual "one inference batch" 1 calls
      case N.previous observed of
        Just (Right prepared) -> do
          assertBool "original available on Jev failure"
            ("An observation worth returning to." `T.isInfixOf` A.rendered prepared)
          assertBool "diagnostics retained" ("JevCircuitOpen" `T.isInfixOf` T.pack (show (A.snapshot prepared)))
        other -> fail (show other)
  , testCase "reframing preserves the actual previous preparation" $ do
      let (first, _) = runNotebook [N.Budget (A.Limits 0 0), N.Prepare]
          (revised, calls) = runNotebook [N.Budget (A.Limits 0 0), N.Prepare, N.Occasion "Another angle"]
      assertEqual "retained result unchanged" (N.previous first) (N.previous revised)
      assertBool "marked as changed" (N.changedSincePreparation revised)
      assertEqual "revision is not another inference" 0 calls
  , testCase "human form returns the interpreter's original instructions" $ do
      let form = prepareForm (N.framing "Earlier occasion")
      case decodeSubmission form (object ["f0" .= ("Look at this together" :: Text)]) of
        Left problem -> fail (show problem)
        Right commands -> do
          let (observed, calls) = runNotebook (N.Budget (A.Limits 0 0) : commands)
          assertEqual "submitted occasion" "Look at this together" (N.occasion observed)
          assertBool "form also requests preparation" (case N.previous observed of Just (Right _) -> True; _ -> False)
          assertEqual "respects current budget" 0 calls
  , testCase "an ordinary Haskell view function changes the prepared presentation" $ do
      let template values = let initial = A.continuity values in initial
            { A.nodes = [n {A.fallback = (A.fallback n) {A.text = "A question to carry: " <> A.text (A.fallback n)}} | n <- A.nodes initial] }
          (observed, _) = runNotebook [N.View template, N.Budget (A.Limits 0 0), N.Prepare]
      case N.previous observed of
        Just (Right prepared) -> assertBool "template participates in preparation"
          ("A question to carry:" `T.isInfixOf` A.rendered prepared)
        other -> fail (show other)
  , testCase "invalid preparation is observable and performs no inference" $ do
      let (observed, calls) = runNotebook [N.Budget (A.Limits (-1) 0), N.Prepare]
      assertEqual "typed refusal" (Just (Left A.InvalidLimits)) (N.previous observed)
      assertEqual "no inference" 0 calls
  , testCase "startup preserves memory and requests inspection persistence" $ do
      let files = Map.fromList [("memory/persona.md", "A chosen beginning.")]
          (seed, saved) = run $ runState files $ interpret startupJev $ interpret writeHost $
            interpret readHost (O.startup (StartupInput "startup-operation" Nothing))
      assertBool "existing memory reaches context" ("A chosen beginning." `T.isInfixOf` renderContextSeed seed)
      assertEqual "memory stays authored" (Map.lookup "memory/persona.md" files) (Map.lookup "memory/persona.md" saved)
      -- encodeValue is a hosted intrinsic. This GHC test establishes the write
      -- effect; the production value builder is exercised separately below.
      assertBool "inspection write requested" (Map.member "notes/initial-attention.md" saved)
  , testCase "startup inspection retains operation originals and judgment evidence" $ do
      let files = Map.fromList [("memory/persona.md", "A chosen beginning.")]
          (result, _) = run $ runState files $ interpret startupJev $ interpret writeHost $
            interpret readHost (O.prepare O.arrival)
      case result of
        Left problem -> fail (show problem)
        Right (loaded, prepared) -> do
          let evidence = T.pack (show (O.inspection (StartupInput "startup-operation" Nothing) O.arrival loaded prepared))
          mapM_ (\expected -> assertBool (T.unpack expected) (expected `T.isInfixOf` evidence))
            ["startup-operation", "A chosen beginning.", "JevCircuitOpen"]
  , testCase "failed inspection persistence does not return context for publication" $ do
      let (seed, _) = run $ runState (Map.empty :: Files) $ interpret startupJev $
            interpret writeRefusingInspection $ interpret readHost $
              O.startup (StartupInput "failed-startup" Nothing)
      outcome <- try (evaluate (T.length (renderContextSeed seed))) :: IO (Either SomeException Int)
      case outcome of
        Left problem -> assertBool "retains storage failure"
          ("inspection write refused" `T.isInfixOf` T.pack (show problem))
        Right _ -> fail "startup returned context after failing to preserve its evidence"
  , testCase "absent startup publication is refused without reading or writing context" $ do
      let prepared = either (error . show) id $ run $ interpret noJev $
            Context.prepareView (A.Limits 0 0) "test" collection (A.continuity collection)
          owner :: ContextReadWrite a -> Eff '[] a
          owner GetInitialContextWith = pure C.InitialContextAbsent
          owner _ = error "unpublished context must not be read or edited"
      assertEqual "owner observation retained"
        (Left (Context.InitialContextUnavailable C.InitialContextAbsent))
        (run (interpret owner (Context.activateInitial prepared)))
  ]
