{-# LANGUAGE DataKinds, DeriveGeneric, GADTs, OverloadedStrings, ScopedTypeVariables, TypeApplications, TypeOperators #-}
module Main (main, tests) where

import Control.Exception (evaluate)
import Control.Monad (unless)
import Control.Monad.Freer (Eff, interpret, reinterpret, run, send)
import Control.Monad.Freer.State (State, get, modify, runState)
import Data.List (isInfixOf)
import Data.Text (Text)
import GHC.Generics (Generic)
import System.Directory (createDirectoryIfMissing)
import System.Exit (ExitCode (..))
import System.Process (readProcessWithExitCode)
import Tidepool.Agent.Ref.Internal (internalAgentRef)
import qualified Tidepool.Agent.Reply.Internal as Reply
import qualified Tidepool.Agent.Watch.Internal as Watch
import Tidepool.Agent.Contract
import Tidepool.Aeson.Value (Value (..), ToJSON (toJSON), object, (.=))
import qualified Tidepool.Aeson.KeyMap as KM
import Tidepool.Effects.Core (AgentTools (..), Commands, ContextReadWrite (..))
import Tidepool.Test.Runner

data Tools mode = Tools
  { ordinary :: mode :- Call Text Text
  , curate :: mode :- Sync (Call Text Text)
  , rawCurate :: mode :- Sync (RawCall Text)
  , notifyCurate :: mode :- Sync (Notify Text)
  , notebook :: HaskellTools '[] mode
  } deriving (Generic)

data Narrow mode = Narrow { narrow :: mode :- HaskellCell '[] } deriving (Generic)

data Unpresented mode = Unpresented { unpresented :: mode :- Call Text Text } deriving (Generic)
data UnpresentedRaw mode = UnpresentedRaw { unpresentedRaw :: mode :- RawCall Text } deriving (Generic)
data UnpresentedSync mode = UnpresentedSync { unpresentedSync :: mode :- Sync (Call Text Text) } deriving (Generic)
data UnpresentedSyncRaw mode = UnpresentedSyncRaw { unpresentedSyncRaw :: mode :- Sync (RawCall Text) } deriving (Generic)

data ActorTools mode = ActorTools
  { actorCall :: mode :- Call Text Value
  , actorFinish :: mode :- Finish Text Text
  } deriving (Generic)

data ActorHarness = ActorHarness
  { actorInputs :: [(Text, Value)]
  , actorReplies :: [Value]
  }

actorTools :: ActorTools (AsActorT (Eff '[AgentTools, State ActorHarness]) () Text)
actorTools = ActorTools
  { actorCall = tool "Structured semantic output" (\_ -> pure (object ["meaning" .= ("payload" :: Text)]))
  , actorFinish = finishTool "Finish" (\value -> pure (value, value))
  }

data ContextNotebook mode = ContextNotebook
  { contextNotebook :: mode :- Sync (HaskellCell (SyncEffects '[]))
  } deriving (Generic)

contextNotebookTools :: ContextNotebook (AsServerT (Eff '[]))
contextNotebookTools = ContextNotebook (haskellTool "A context-editing notebook")

narrowTools :: Narrow (AsServerT (Eff '[Commands]))
narrowTools = Narrow (haskellTool "A pure notebook in a command-capable actor")

tools :: Tools (AsServerT (Eff '[]))
tools = Tools
  { ordinary = presentWith id (tool "Echo" pure)
  , curate = presentWith id $ syncTool "Select next model" $ \model -> send (SetNextModelWith model) >> pure model
  , rawCurate = presentWith id (syncRawTool "Echo literal" pure)
  , notifyCurate = presentWith (const "") $ syncNotify "Select next model" (send . SetNextModelWith)
  , notebook = haskellTools
  }

require :: String -> Bool -> IO ()
require label observed = unless observed (error label) >> putStrLn ("passed: " ++ label)

context :: ContextReadWrite a -> Eff '[State [Text]] a
context (SetNextModelWith model) = modify (++ [model])
context _ = error "unexpected context request"

installation :: AgentTools a -> Eff '[State Value, ContextReadWrite] a
installation (AgentToolsInstallWith value _) = modify (const value)
installation _ = error "installation executed the retained handler"

actorRuntime :: AgentTools a -> Eff '[State ActorHarness] a
actorRuntime (AgentToolsAwaitWith _ _ _) = do
  harness <- get
  case actorInputs harness of
    input : remaining -> modify (\current -> current {actorInputs = remaining}) >> pure input
    [] -> error "actor requested an unexpected tool input"
actorRuntime AgentToolsInputWith = error "actor requested an ordinary hosted tool input"
actorRuntime (AgentToolsReplyWith value) = modify (\harness -> harness {actorReplies = actorReplies harness <> [value]})
actorRuntime _ = error "unexpected actor tool operation"

installedToolContracts :: IO ()
installedToolContracts = do
  let missing = compileInstalledTools
        (Unpresented (tool "Echo" pure) :: Unpresented (AsServerT (Eff '[])))
      missingRaw = compileInstalledTools
        (UnpresentedRaw (rawTool "Echo" pure) :: UnpresentedRaw (AsServerT (Eff '[])))
      missingSync = compileInstalledTools
        (UnpresentedSync (syncTool "Echo" pure) :: UnpresentedSync (AsServerT (Eff '[])))
      missingSyncRaw = compileInstalledTools
        (UnpresentedSyncRaw (syncRawTool "Echo" pure) :: UnpresentedSyncRaw (AsServerT (Eff '[])))
      rejected result = case result of Left MissingToolPresentation {} -> True; _ -> False
  require "installed named handlers require explicit presentation"
    (rejected missing)
  require "installed raw handlers require explicit presentation before dispatch"
    (rejected missingRaw)
  require "installed sync handlers require explicit presentation before dispatch"
    (rejected missingSync)
  require "installed sync raw handlers require explicit presentation before dispatch"
    (rejected missingSyncRaw)
  compiled <- either (error . show) pure (compileInstalledTools tools)
  let declared = declarations compiled
      runTool name = run $ runState [] $ reinterpret context $ dispatch compiled name (toJSON ("executor" :: Text))
  require "one traversal preserves field order"
    (map dtdName declared == ["ordinary", "curate", "raw_curate", "notify_curate", "haskell", "haskell_sync"])
  require "async default and explicit sync scheduling"
    (map dtdSchedule declared == [Asynchronous, BeforeNextInference, BeforeNextInference, BeforeNextInference, Asynchronous, BeforeNextInference])
  require "native notebook scheduling preserves the same selected row"
    (map dtdEffectKeys (drop 4 declared) == [Just [], Just []])
  contextCompiled <- either (error . show) pure (compileInstalledTools contextNotebookTools)
  require "context-editing notebooks declare their additional effect explicitly"
    (map (\entry -> (dtdSchedule entry, dtdEffectKeys entry)) (declarations contextCompiled)
      == [(BeforeNextInference, Just ["ContextReadWrite"])])
  require "native endpoints cannot enter the handler dispatcher"
    (fst (runTool "haskell") == Left (NativeToolInvocation "haskell"))
  require "async compiled handler is lifted into shared dispatcher"
    (runTool "ordinary" == (Right (ToolDispatchSuccess (toJSON ("executor" :: Text)) "executor"), []))
  require "dispatch reply keeps semantic output separate from selected presentation"
    (toolDispatchReply (Right (ToolDispatchSuccess (toJSON ("payload" :: Text)) "model text"))
      == object ["status" .= ("success" :: Text), "output" .= ("payload" :: Text), "presentation" .= ("model text" :: Text)])
  let actorInitial = ActorHarness
        [("actor_call", toJSON ("call" :: Text)), ("actor_finish", toJSON ("done" :: Text))]
        []
      (actorExit, actorFinal) = run $ runState actorInitial $ interpret actorRuntime (serveTools actorTools)
  require "programmatic actor tools need no presenter and return structured semantic output"
    (actorExit == "done" && actorReplies actorFinal ==
      [ object ["status" .= ("success" :: Text), "output" .= object ["meaning" .= ("payload" :: Text)]]
      , object ["status" .= ("success" :: Text), "output" .= ("done" :: Text)]
      ])
  require "sync compiled handler emits context effect in shared dispatcher"
    (runTool "curate" == (Right (ToolDispatchSuccess (toJSON ("executor" :: Text)) "executor"), ["executor"]))
  require "sync raw handler reuses compiled function"
    (runTool "raw_curate" == (Right (ToolDispatchSuccess (toJSON ("executor" :: Text)) "executor"), []))
  require "sync notification runs context effect"
    (snd (runTool "notify_curate") == ["executor"])
  asyncDefault <- either (error . show) pure (compileInstalledTools (specTools (defaultAsyncWorkbenchSpec :: AgentSpec (AsyncHaskellTools '[]) '[])))
  require "host without context support declares only the async notebook"
    (map (\entry -> (dtdName entry, dtdSchedule entry, dtdImplementation entry, dtdEffectKeys entry)) (declarations asyncDefault)
      == [("haskell", Asynchronous, NativeHaskellCell, Just [])])
  narrowCompiled <- either (error . show) pure (compileInstalledTools narrowTools)
  require "notebook profile may select a strict subset of actor effects"
    (map dtdEffectKeys (declarations narrowCompiled) == [Just []])
  let spec = defaultSpec {specTools = tools, afterTool = Just (\_ _ -> pure NoAnnotation)}
      (_, manifest) = run $ interpret (\(_ :: ContextReadWrite a) -> error "bootstrap context access") $
        runState Null $ reinterpret installation $ installSpec spec
      installed = case field "tools" manifest of Just (Array values) -> values; _ -> []
  require "installation resolves sync profile and async hook profile"
    (field "slotEffectKeys" manifest == Just (object ["afterTool" .= ([] :: [Text])]) &&
      map (field "effectKeys") installed == map (Just . toJSON)
        ([[], ["ContextReadWrite"], ["ContextReadWrite"], ["ContextReadWrite"], [], []] :: [[Text]]))
  require "installed manifest includes implementation identity"
    (map (field "implementation") (drop 4 installed) == replicate 2 (Just (toJSON ("haskell_cell" :: Text))))
  where
    field name (Object values) = KM.lookup (KM.fromText name) values
    field _ _ = Nothing

-- Exercise the retained Haskell projection with the ordinary effect interpreter.
-- The real host release/custody control lives in the resident integration test.
observeOnce
  :: Watch.RawWatchObservation -> Watch.Await a
  -> (Either Watch.AwaitError a, [Watch.AwaitPlan])
observeOnce observation awaiting = run $ runState [] $ interpret host (Watch.await awaiting)
  where
    host :: Watch.Watches value -> Eff '[State [Watch.AwaitPlan]] value
    host (Watch.RegisterAwaitWith plan) = modify (++ [plan]) >> pure (Right 73)
    host (Watch.AwaitWatchWith 73) = pure observation
    host (Watch.ForgetWatchWith 73) = pure Watch.WatchForgotten
    host _ = error "unexpected receipt observation effect"

retainedReceiptProjection :: IO ()
retainedReceiptProjection = do
  let requestId = Reply.RequestId 17
      (pending, _) = Reply.newRequestHandles () requestId (internalAgentRef 23 29)
      payload = (map (+ 1) [1 .. 400 :: Int], (\n -> n + 37) :: Int -> Int)
      original = Reply.ResponseResult payload (Reply.ExecutionReceipt requestId 23 29) Reply.NoBoundWorktree
      decision = Watch.RawWatchReady (Watch.AwaitDecision [(0, Nothing)] [])
  _ <- evaluate (Reply.fillResponse pending original)
  let (received, fullPlan) = observeOnce decision (Watch.response pending)
      (value, valuePlan) = observeOnce decision (Watch.result pending)
      (settled, settledPlan) = observeOnce decision (Watch.settledResponse pending)
      (_, settlementPlan) = observeOnce decision (Watch.settlement pending)
  require "value projections retain the same response plans"
    (fullPlan == valuePlan && settledPlan == settlementPlan
      && fullPlan == [Watch.AwaitPlan [Watch.LeafNode (Watch.AwaitDependency requestId False)] 0]
      && settledPlan == [Watch.AwaitPlan [Watch.LeafNode (Watch.AwaitDependency requestId True)] 0])
  case (received, value, settled) of
    (Right receipt, Right (values, apply), Right (Right settledReceipt)) -> do
      require "receipt projection preserves original execution and worktree evidence"
        (Reply.responseExecution receipt == Reply.responseExecution original
          && Reply.responseWorktree receipt == Reply.responseWorktree original
          && Reply.responseExecution settledReceipt == Reply.responseExecution original)
      let (retainedValues, retainedApply) = Reply.responseValue receipt
          (settledValues, settledApply) = Reply.responseValue settledReceipt
      require "lazy data and closures remain usable after transient observation release"
        (values == [2 .. 401] && retainedValues == values && settledValues == values
          && apply 5 == 42 && retainedApply 6 == 43 && settledApply 7 == 44)
    _ -> error "successful receipt projection was not retained"

receiptFailureProjection :: IO ()
receiptFailureProjection = do
  let requestId = Reply.RequestId 19
      (pending, _) = Reply.newRequestHandles () requestId (internalAgentRef 31 41)
      failure = Reply.ResponseTargetFailed "controlled terminal failure"
      unavailable = Watch.RawWatchUnavailable requestId failure
      captured = Watch.RawWatchReady (Watch.AwaitDecision [(0, Just failure)] [])
      (full, _) = observeOnce unavailable (Watch.response (pending :: Reply.Request Int))
      (value, _) = observeOnce unavailable (Watch.result pending)
      (settled, _) = observeOnce captured (Watch.settledResponse pending)
      (settledValue, _) = observeOnce captured (Watch.settlement pending)
      rejected = Watch.AwaitRejected Reply.ReplyUnauthorized
      (refused, _) = observeOnce (Watch.RawWatchRejected Reply.ReplyUnauthorized) (Watch.settledResponse pending)
  require "successful-only receipt and value projections preserve AwaitError"
    (full == Left (Watch.AwaitDependencyUnavailable requestId failure)
      && value == Left (Watch.AwaitDependencyUnavailable requestId failure))
  require "settled receipt and value projections capture the exact terminal failure"
    (settled == Right (Left failure) && settledValue == Right (Left failure))
  require "settled receipt does not turn observation rejection into terminal failure"
    (refused == Left rejected)

data CompileExpectation = Accepted | Rejected [String]

compileProfile :: String -> CompileExpectation -> IO ()
compileProfile fixture expectation = do
  support <- requiredInput "TIDEPOOL_TEST_EFFECTS_DIR"
  let output = "profile-fixture-objects/" ++ fixture
  createDirectoryIfMissing True output
  (status, out, err) <- readProcessWithExitCode "ghc"
    [ "-fno-code", "-fforce-recomp", "-i" ++ support
    , "-ilib", "-iactors"
    , "-outputdir", output, "test-check/tool-profiles/" ++ fixture ++ ".hs"
    ] ""
  writeFile (output ++ "/compile.log") (out ++ err)
  case expectation of
    Accepted -> require (fixture ++ " compiles\n" ++ out ++ err) (status == ExitSuccess)
    Rejected fragments -> require (fixture ++ " rejects at its intended type boundary\n" ++ out ++ err)
      (status /= ExitSuccess && all (`isInfixOf` (out ++ err)) fragments)

tests :: TestTree
tests = testGroup "tool profile contract"
  [ testCase "receipt projections preserve original evidence and lazy payloads" retainedReceiptProjection
  , testCase "receipt projections distinguish terminal failure from observation rejection" receiptFailureProjection
  , testCase "compiled installation dispatch scheduling presentation and actor profiles" installedToolContracts
  , testCase "ordinary compiled model program can be raised into sync context" $
      compileProfile "RaisedModelTurn" Accepted
  , testCase "sync context admits explicit effort changes" $
      compileProfile "ContextEffort" Accepted
  , testCase "async row cannot change next model" $
      compileProfile "AsyncContext" (Rejected ["is not a member of the type-level list"])
  , testCase "async row cannot change next effort" $
      compileProfile "AsyncContextEffort" (Rejected ["is not a member of the type-level list"])
  , testCase "async profile cannot declare a sync context effect" $
      compileProfile "AsyncProfile" (Rejected ["cannot be used by an asynchronous tool"])
  , testCase "notebook profile cannot borrow unsupported command authority" $
      compileProfile "UnsupportedProfile" (Rejected ["No instance for", "Contains Commands"])
  , testCase "concrete sync handler cannot masquerade as empty async profile" $
      compileProfile "ConcreteSyncHandler" (Rejected ["LiftTool"])
  , testCase "concrete native notebook cannot borrow another actor command row" $
      compileProfile "ConcreteNativeProfile" (Rejected ["Commands"])
  , testCase "model hook cannot borrow caller sync context authority" $
      compileProfile "ModelContext" (Rejected ["cannot be used by an asynchronous tool"])
  ]

main :: IO ()
main = runTests tests
