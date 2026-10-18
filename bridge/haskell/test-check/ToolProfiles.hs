{-# LANGUAGE DataKinds, DeriveGeneric, GADTs, OverloadedStrings, ScopedTypeVariables, TypeApplications, TypeOperators #-}
module Main (main, tests) where

import Control.Monad (unless)
import qualified Tidepool.Actors.Spawn as Spawn
import qualified Tidepool.Model.Selection as Model
import Tidepool.Actors.Workbench ()
import qualified Tidepool.Effects.Core as Core
import Control.Monad.Freer (Eff, interpret, reinterpret, run, send)
import Control.Monad.Freer.State (State, get, modify, runState)
import Data.List (isInfixOf)
import Data.Text (Text)
import GHC.Generics (Generic)
import QuantitiesContract (quantitiesTests)
import LookupNavigationTest (lookupNavigationTests)
import System.Directory (createDirectoryIfMissing)
import System.Exit (ExitCode (..))
import System.Process (readProcessWithExitCode)
import Tidepool.Agent.Ref.Internal (internalAgentRef)
import qualified Tidepool.Agent.Reply.Internal as Reply
import qualified Tidepool.Agent.Watch.Internal as Watch
import Tidepool.Agent.Contract
import Tidepool.Aeson.Value (Value (..), ToJSON (toJSON), object, (.=))
import qualified Tidepool.Aeson.KeyMap as KM
import Tidepool.Effects.Core (AgentTools (..), Commands, ContextReadWrite (..), Reload)
import Tidepool.Internal.ActorProfiles (ActorEffects)
import Tidepool.Inspection.Display (WorkbenchDisplay (..))
import Tidepool.Test.Runner

data Tools mode = Tools
  { ordinary :: mode :- Call Text Text
  , curate :: mode :- Sync (Call Text Text)
  , rawCurate :: mode :- Sync (RawCall Text)
  , notifyCurate :: mode :- Sync (Notify Text)
  , notebook :: HaskellTools '[] mode
  } deriving (Generic)

data Narrow mode = Narrow { narrow :: mode :- HaskellCell '[] } deriving (Generic)

data SemanticTools mode = SemanticTools
  { semanticCall :: mode :- Call Text Text
  , semanticRaw :: mode :- RawCall Text
  , semanticNotify :: mode :- Notify Text
  } deriving (Generic)

data NestedTools mode = NestedTools
  { semantic :: SemanticTools mode
  } deriving (Generic)

semanticTools :: NestedTools (AsServerT (Eff '[]))
semanticTools = NestedTools $ SemanticTools
  { semanticCall = presentWith (const "selected call") $ tool "Echo semantic text" pure
  , semanticRaw = presentWith (const "selected raw") $ rawTool "Echo literal text" pure
  , semanticNotify = presentWith (const "selected notice") $ notify "Accept notification" (const (pure ()))
  }

data ActorTools mode = ActorTools
  { actorCall :: mode :- Call Text Value
  , actorRaw :: mode :- RawCall Text
  , actorNotify :: mode :- Notify Text
  , actorUpdate :: mode :- Update Text Text
  , actorFinish :: mode :- Finish Text Text
  } deriving (Generic)

data ActorHarness = ActorHarness
  { actorInputs :: [(Text, Value)]
  , actorReplies :: [Value]
  }

actorTools :: Text -> ActorTools (AsActorT (Eff '[AgentTools, State ActorHarness]) Text Text)
actorTools state = ActorTools
  { actorCall = tool "Structured semantic output" (\_ -> pure (object ["meaning" .= ("payload" :: Text)]))
  , actorRaw = rawTool "Literal semantic output" pure
  , actorNotify = notify "Semantic notification" (const (pure ()))
  , actorUpdate = updateTool "Replace resident state" (\value -> pure (value, value))
  , actorFinish = finishTool "Finish with resident state" (\value -> pure (state, value))
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
  , curate = presentWith (const "selected sync call") $ syncTool "Select next model" $ \model -> send (SetNextModelWith (Core.CustomModel Core.OpenAIPlan model)) >> pure model
  , rawCurate = presentWith (const "selected sync raw") (syncRawTool "Echo literal" pure)
  , notifyCurate = presentWith (const "selected sync notice") $ syncNotify "Select next model" (send . SetNextModelWith . Core.CustomModel Core.OpenAIPlan)
  , notebook = haskellTools
  }

require :: String -> Bool -> IO ()
require label observed = unless observed (error label) >> putStrLn ("passed: " ++ label)

context :: ContextReadWrite a -> Eff '[State [Core.Model], Reload] a
context (SetNextModelWith model) = modify (++ [model])
context _ = error "unexpected context request"

noReload :: Reload a -> Eff effects a
noReload _ = error "unexpected reload request"

installation :: AgentTools a -> Eff '[State Value, ContextReadWrite, Reload] a
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
  compiled <- either (error . show) pure (compileInstalledTools tools)
  let declared = declarations compiled
      runTool name = run $ interpret noReload $ runState [] $ reinterpret context $ dispatch compiled name (toJSON ("executor" :: Text))
  require "one traversal preserves field order"
    (map dtdName declared == ["ordinary", "curate", "raw_curate", "notify_curate", "haskell", "haskell_sync"])
  require "async default and explicit sync scheduling"
    (map dtdSchedule declared == [Asynchronous, BeforeNextInference, BeforeNextInference, BeforeNextInference, Asynchronous, BeforeNextInference])
  require "native notebook scheduling preserves the same selected row"
    (map dtdEffectKeys (drop 4 declared) == [Just [], Just []])
  let runIndex index = run $ interpret noReload $ runState [] $ reinterpret context $ dispatchAt compiled index (toJSON ("executor" :: Text))
  require "installed ordinal dispatch matches declared order"
    (and [runIndex index == runTool (dtdName declaration) | (index, declaration) <- zip [0..] declared])
  require "invalid installed ordinal refuses without running a handler"
    (case runIndex (-1) of (Left (UnknownTool _), []) -> True; _ -> False)
  contextCompiled <- either (error . show) pure (compileInstalledTools contextNotebookTools)
  require "context-editing notebooks declare their additional effect explicitly"
    (map (\entry -> (dtdSchedule entry, dtdEffectKeys entry)) (declarations contextCompiled)
      == [(BeforeNextInference, Just ["ContextReadWrite", "Reload"])])
  require "native endpoints cannot enter the handler dispatcher"
    (fst (runTool "haskell") == Left (NativeToolInvocation "haskell"))
  require "async compiled handler is lifted into shared dispatcher"
    (runTool "ordinary" == (Right (ToolDispatchSuccess (toJSON ("executor" :: Text)) "executor"), []))
  require "dispatch reply keeps semantic output separate from selected presentation"
    (toolDispatchReply (Right (ToolDispatchSuccess (toJSON ("payload" :: Text)) "model text"))
      == object ["status" .= ("success" :: Text), "output" .= ("payload" :: Text), "presentation" .= ("model text" :: Text)])
  let actorInitial = ActorHarness
        [ ("actor_call", toJSON ("call" :: Text))
        , ("actor_raw", toJSON ("literal" :: Text))
        , ("actor_notify", toJSON ("notice" :: Text))
        , ("actor_update", toJSON ("updated" :: Text))
        , ("actor_finish", toJSON ("done" :: Text))
        ]
        []
      (actorExit, actorFinal) = run $ runState actorInitial $ interpret actorRuntime (serveToolsWith "initial" actorTools)
  require "programmatic actor tools need no presenter and return structured semantic output"
    (actorExit == "done" && actorReplies actorFinal ==
      [ object ["status" .= ("success" :: Text), "output" .= object ["meaning" .= ("payload" :: Text)]]
      , object ["status" .= ("success" :: Text), "output" .= ("literal" :: Text)]
      , object ["status" .= ("success" :: Text), "output" .= ()]
      , object ["status" .= ("success" :: Text), "output" .= ("updated" :: Text)]
      , object ["status" .= ("success" :: Text), "output" .= ("updated" :: Text)]
      ])
  require "sync compiled handler emits context effect in shared dispatcher"
    (runTool "curate" == (Right (ToolDispatchSuccess (toJSON ("executor" :: Text)) "selected sync call"), [Core.CustomModel Core.OpenAIPlan "executor"]))
  require "sync raw handler reuses compiled function"
    (runTool "raw_curate" == (Right (ToolDispatchSuccess (toJSON ("executor" :: Text)) "selected sync raw"), []))
  require "sync notification preserves selected presentation and context effect"
    (runTool "notify_curate" == (Right (ToolDispatchSuccess (toJSON ()) "selected sync notice"), [Core.CustomModel Core.OpenAIPlan "executor"]))
  asyncDefault <- either (error . show) pure (compileInstalledTools (specTools (defaultAsyncWorkbenchSpec :: AgentSpec (AsyncHaskellTools '[]) '[])))
  require "host without context support declares only the async notebook"
    (map (\entry -> (dtdName entry, dtdSchedule entry, dtdImplementation entry, dtdEffectKeys entry)) (declarations asyncDefault)
      == [("haskell", Asynchronous, NativeHaskellCell, Just [])])
  actorDefault <- either (error . show) pure (compileInstalledTools
    (specTools (defaultAsyncWorkbenchSpec :: AgentSpec (AsyncHaskellTools ActorEffects) ActorEffects)))
  require "default actor notebook reflects form and concurrency effects without sync authority"
    (case declarations actorDefault of
      [entry] -> dtdName entry == "haskell" && dtdSchedule entry == Asynchronous
        && dtdImplementation entry == NativeHaskellCell
        && case dtdEffectKeys entry of
          Just keys -> length (filter (== "AskUser") keys) == 1
            && length (filter (== "Green") keys) == 1 && "ContextReadWrite" `notElem` keys
          Nothing -> False
      _ -> False)
  narrowCompiled <- either (error . show) pure (compileInstalledTools narrowTools)
  require "notebook profile may select a strict subset of actor effects"
    (map dtdEffectKeys (declarations narrowCompiled) == [Just []])
  let spec = defaultSpec {specTools = tools, afterTool = Just (\_ _ -> pure NoAnnotation)}
      (_, manifest) = run $ interpret noReload $ interpret (\(_ :: ContextReadWrite a) -> error "bootstrap context access") $
        runState Null $ reinterpret installation $ installSpec spec
      installed = case field "tools" manifest of Just (Array values) -> values; _ -> []
  require "installation resolves sync profile and async hook profile"
    (field "slotEffectKeys" manifest == Just (object ["afterTool" .= ([] :: [Text])]) &&
      map (field "effectKeys") installed == map (Just . toJSON)
        ([[], ["ContextReadWrite", "Reload"], ["ContextReadWrite", "Reload"], ["ContextReadWrite", "Reload"], [], []] :: [[Text]]))
  require "installed manifest includes implementation identity"
    (map (field "implementation") (drop 4 installed) == replicate 2 (Just (toJSON ("haskell_cell" :: Text))))
  where
    field name (Object values) = KM.lookup (KM.fromText name) values
    field _ _ = Nothing

preparedSpecContracts :: IO ()
preparedSpecContracts = do
  let spec = defaultSpec
        { specTools = tools
        , afterTool = Just (error "preparation executed after-tool")
        , initialContext = Just (error "preparation executed startup")
        }
  prepared <- either (error . show) pure (prepareAgentSpec spec)
  let metadata = preparedAgentSpecMetadata prepared
      (_, installed) = run $ interpret noReload $ interpret (\(_ :: ContextReadWrite a) -> error "unexpected context request") $
        runState Null $ reinterpret installation $ installPreparedSpec prepared
  require "preparation and installation share exactly one declaration set" (metadata == installed)
  let model = Model.openAI "test-catalog" "primary" (Model.OpenAIOptions Model.OpenAILow)
      refused = run $ interpret noSpawn $ interpret refuse $
        Spawn.spawnSubagent (Spawn.FreshCtx "fixture") Spawn.SameDir (Spawn.defaultSpawnOptions model spec)
  require "public scalar preflight refusal does not emit a child allocation" (case refused of
    Left (Spawn.SpawnInvalidSpec (Core.SpecInputSchemaInvalid _)) -> True
    _ -> False)
  where
    refuse :: Core.AgentLaunch a -> Eff '[Core.SpawnAgent Core.OpenAI] a
    refuse (Core.AgentLaunchValidateSpecWith _ _) = pure (Left (Core.SpecInputSchemaInvalid "function input must be an object"))
    refuse _ = error "refused preflight reached another administrative effect"
    noSpawn :: Core.SpawnAgent Core.OpenAI a -> Eff '[] a
    noSpawn _ = error "refused preflight reached a child allocation"


presentedCompositionContracts :: IO ()
presentedCompositionContracts = do
  bounded <- either (error . show) pure (compileTools semanticTools)
  installed <- either (error . show) pure (compileInstalledTools semanticTools)
  let names = ["semantic_call", "semantic_raw", "semantic_notify"]
      input = toJSON ("payload" :: Text)
      expected =
        [ Right (ToolDispatchSuccess input "selected call")
        , Right (ToolDispatchSuccess input "selected raw")
        , Right (ToolDispatchSuccess (toJSON ()) "selected notice")
        ]
      inRow = run (traverse (\name -> dispatch bounded name input) names)
      raised = run $ interpret noReload $ interpret (\(_ :: ContextReadWrite a) -> error "unexpected context request") $
        traverse (\name -> dispatch installed name input) names
  require "nested presentation composes under bounded and installed compilation"
    (declarations bounded == declarations installed && map dtdName (declarations bounded) == names)
  require "nested call raw and notification retain independent semantics and presentation"
    (inRow == expected && raised == expected)

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

receiptFailureProjection :: IO ()
receiptFailureProjection = do
  let requestId = Reply.RequestId 19
      ((pending :: Reply.Request Int), _) = Reply.newRequestHandles requestId (internalAgentRef 31 41)
      failure = Reply.ResponseTargetFailed "controlled terminal failure"
      unavailable = Watch.RawWatchUnavailable requestId failure
      captured = Watch.RawWatchReady (Watch.AwaitDecision [(0, Just failure)] [])
      (full, _) = observeOnce unavailable (Watch.response pending)
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
compileProfile fixture = compileAt fixture ("test-check/tool-profiles/" ++ fixture ++ ".hs")

compileAt :: String -> FilePath -> CompileExpectation -> IO ()
compileAt fixture source expectation = do
  support <- requiredInput "TIDEPOOL_TEST_EFFECTS_DIR"
  let output = "profile-fixture-objects/" ++ fixture
  createDirectoryIfMissing True output
  (status, out, err) <- readProcessWithExitCode "ghc"
    [ "-fno-code", "-fforce-recomp", "-i" ++ support
    , "-ilib", "-iactors"
    , "-outputdir", output, source
    ] ""
  writeFile (output ++ "/compile.log") (out ++ err)
  case expectation of
    Accepted -> require (fixture ++ " compiles\n" ++ out ++ err) (status == ExitSuccess)
    Rejected fragments -> require (fixture ++ " rejects at its intended type boundary\n" ++ out ++ err)
      (status /= ExitSuccess && all (`isInfixOf` (out ++ err)) fragments)

presentationTests :: [TestTree]
presentationTests =
  [ testCase (name ++ " requires presentation at construction") $
      compileProfile name (Rejected ["Couldn't match", "Presented"])
  | name <-
      [ "MissingCallPresentation", "MissingRawPresentation", "MissingNotifyPresentation"
      , "MissingSyncPresentation", "MissingSyncRawPresentation", "MissingSyncNotifyPresentation"
      ]
  ] ++
  [ testCase (name ++ policy ++ " cannot bypass hosted presentation") $
      compileProfile (name ++ policy) (Rejected ["GCompileTools"])
  | name <- ["ConcreteCall", "ConcreteRaw"]
  , policy <- ["Bounded", "Installed"]
  ] ++
  [ testCase "nested handler requires presentation at construction" $
      compileProfile "MissingNestedPresentation" (Rejected ["Couldn't match", "Presented"])
  , testCase "renderer must consume the handler output type" $
      compileProfile "WrongPresentationOutput" (Rejected ["Couldn't match", "Int", "Text"])
  , testCase "presentation completion composes with generic helpers and bounded model turns" $
      compileProfile "GenericPresentedModel" Accepted
  ]

endpointIndexTests :: [TestTree]
endpointIndexTests =
  [ testCase (name ++ " rejects at the endpoint index") $ compileProfile name (Rejected ["Couldn't match", "CallKind", "NotifyKind"])
  | name <- ["NotifyAsCall", "CallAsNotify", "SyncNotifyAsCall", "SyncCallAsNotify", "CoercedNotifyAsCall"]
  ] ++
  [ testCase "finish cannot impersonate a unit call" $ compileProfile "FinishAsCall" (Rejected ["Couldn't match", "FinishTool", "Tool"])
  , testCase "unit calls and notifications retain distinct implementations" $ compileProfile "UnitEndpoints" Accepted
  ]

tests :: TestTree
tests = testGroup "tool profile contract" $
  [ testCase "built-in installers retain their actor row and response and watch displays" $ do
      compileProfile "BuiltinInstallers" Accepted
      require "actor installer brings the specialized response display"
        (workbenchDisplay (Reply.ResponseStarting "waiting" :: Reply.ResponseState Int) == ("starting: waiting", False))
      require "actor installer brings the specialized watch display without forcing its result"
        (workbenchDisplay (Watch.WatchReady (error "display forced a watched result") :: Watch.WatchState Int) == ("WatchReady", True))
  , testCase "prepared spec metadata does not execute handlers or slots and refusal cannot launch" preparedSpecContracts
  , testCase "concrete spec preparation requires an input schema dictionary" $ compileProfile "PreparedSpecMissingSchema" (Rejected ["JsonSchema Input"])
  , lookupNavigationTests
  , testCase "receipt projections distinguish terminal failure from observation rejection" receiptFailureProjection
  , testCase "compiled installation dispatch scheduling presentation and actor profiles" installedToolContracts
  , testCase "nested bounded and installed handlers preserve semantic presentation" presentedCompositionContracts
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
  ] ++ presentationTests ++ endpointIndexTests ++ quantitiesTests

main :: IO ()
main = runTests tests
