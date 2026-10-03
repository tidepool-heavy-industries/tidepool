{-# LANGUAGE DataKinds, DeriveGeneric, GADTs, OverloadedStrings, ScopedTypeVariables, TypeApplications, TypeOperators #-}
module Main where

import Control.Monad (unless)
import Control.Monad.Freer (Eff, interpret, reinterpret, run, send)
import Control.Monad.Freer.State (State, modify, runState)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract
import Tidepool.Aeson.Value (Value (..), ToJSON (toJSON), object, (.=))
import qualified Tidepool.Aeson.KeyMap as KM
import Tidepool.Effects.Core (AgentTools (..), Commands, ContextReadWrite (..))

data Tools mode = Tools
  { ordinary :: mode :- Call Text Text
  , curate :: mode :- Sync (Call Text Text)
  , rawCurate :: mode :- Sync (RawCall Text)
  , notifyCurate :: mode :- Sync (Notify Text)
  , notebook :: HaskellTools '[] mode
  } deriving (Generic)

data Narrow mode = Narrow { narrow :: mode :- HaskellCell '[] } deriving (Generic)

data Unpresented mode = Unpresented { unpresented :: mode :- Call Text Text } deriving (Generic)

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

main :: IO ()
main = do
  let missing = compileInstalledTools
        (Unpresented (tool "Echo" pure) :: Unpresented (AsServerT (Eff '[])))
  require "installed named handlers require explicit presentation"
    (case missing of Left MissingToolPresentation {} -> True; _ -> False)
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
