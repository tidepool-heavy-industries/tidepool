{-# LANGUAGE DataKinds, DeriveGeneric, GADTs, OverloadedStrings, ScopedTypeVariables, TypeApplications, TypeOperators #-}
module Main (main, tests) where

import Control.Monad (unless)
import Control.Monad.Freer (Eff, interpret, reinterpret, run, send)
import Control.Monad.Freer.State (State, get, modify, runState)
import Data.List (isInfixOf)
import Data.Text (Text)
import GHC.Generics (Generic)
import QuantitiesContract (quantitiesTests)
import System.Directory (createDirectoryIfMissing)
import System.Exit (ExitCode (..))
import System.Process (readProcessWithExitCode)
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
  , curate = presentWith (const "selected sync call") $ syncTool "Select next model" $ \model -> send (SetNextModelWith model) >> pure model
  , rawCurate = presentWith (const "selected sync raw") (syncRawTool "Echo literal" pure)
  , notifyCurate = presentWith (const "selected sync notice") $ syncNotify "Select next model" (send . SetNextModelWith)
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
    (runTool "curate" == (Right (ToolDispatchSuccess (toJSON ("executor" :: Text)) "selected sync call"), ["executor"]))
  require "sync raw handler reuses compiled function"
    (runTool "raw_curate" == (Right (ToolDispatchSuccess (toJSON ("executor" :: Text)) "selected sync raw"), []))
  require "sync notification preserves selected presentation and context effect"
    (runTool "notify_curate" == (Right (ToolDispatchSuccess (toJSON ()) "selected sync notice"), ["executor"]))
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
      raised = run $ interpret (\(_ :: ContextReadWrite a) -> error "unexpected context request") $
        traverse (\name -> dispatch installed name input) names
  require "nested presentation composes under bounded and installed compilation"
    (declarations bounded == declarations installed && map dtdName (declarations bounded) == names)
  require "nested call raw and notification retain independent semantics and presentation"
    (inRow == expected && raised == expected)

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
      compileProfile name (Rejected ["match type", "Presented"])
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
      compileProfile "MissingNestedPresentation" (Rejected ["match type", "Presented"])
  , testCase "renderer must consume the handler output type" $
      compileProfile "WrongPresentationOutput" (Rejected ["match type", "Int", "Text"])
  , testCase "presentation completion composes with generic helpers and bounded model turns" $
      compileProfile "GenericPresentedModel" Accepted
  ]

tests :: TestTree
tests = testGroup "tool profile contract" $
  [ testCase "compiled installation dispatch scheduling presentation and actor profiles" installedToolContracts
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
  ] ++ presentationTests ++ quantitiesTests

main :: IO ()
main = runTests tests
