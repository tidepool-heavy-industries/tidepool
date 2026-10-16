{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
module PersonaSeedTest (personaSeedTests) where
import Prelude
import Data.Text (Text)
import Data.Proxy (Proxy(..))
import Tidepool.Agent.ToolEffects (toolEffectNames)
import Tidepool.Effects.Row (effectKeys, knownEffects)
import qualified Data.Map.Strict as Map
import Control.Monad.Freer (Eff, interpret, run)
import Control.Monad.Freer.State (State, get, modify, runState)
import Tidepool.Test.Runner (TestTree, testGroup, testCase)
import Test.Tasty.HUnit (assertEqual, assertBool)
import Tidepool.Aeson
import Tidepool.Effects.Core (FsWrite(..), FsWriteReceipt(..), FsPublication(..), Web(..), Jev(..), JevCallError(..), Reflect(..), ReflectError(..))
import Tidepool.Internal.ActorProfiles (ActorEffects, actorEffectName)
import Tidepool.Form.Wire (prepareForm, decodeSubmission)
import qualified Tidepool.Form as F
import qualified Tidepool.View as V
import qualified Persona.Memory as M
import qualified Persona.Web as W
import qualified Persona.Spec as Spec
import qualified Tidepool.Agent.Contract as Contract

writeHost :: FsWrite a -> Eff '[State (Map.Map Text Text)] a
writeHost (FsWrite file body) = modify (Map.insert file body) >> pure (Right ())
writeHost (FsWriteCas file expected body) = do
  files <- get @(Map.Map Text Text)
  case (Map.lookup file files, expected) of
    (Nothing, Nothing) -> modify (Map.insert file body) >> pure (Right (Right (FsWriteReceipt file "published-version" FsDurable)))
    _ -> pure (Right (Left (Just "existing-file-hash")))

jevUnavailable :: Jev a -> Eff '[] a
jevUnavailable (JevAskWith _) = pure (Left (JevCircuitOpen 503 987))
reflectionUnavailable :: Reflect a -> Eff '[Jev] a
reflectionUnavailable (ReflectWith _) = error "explicit-intent selection read conversation history"
reflectionFour :: Reflect a -> Eff '[Jev] a
reflectionFour (ReflectWith count)
  | count == 4 = pure (Left ReflectUnbound)
  | otherwise = error "recent context requested a different turn count"

searchHost :: Web a -> Eff '[] a
searchHost (WebSearch query count includeDomains excludeDomains)
  | query == "query" && count == 3 && null includeDomains && null excludeDomains =
      pure (Right (toJSON [W.SearchHit "title" "https://example.org" "excerpt"]))
  | otherwise = error "search arguments changed"
searchHost _ = error "search invoked a different web operation"

personaSeedTests :: TestTree
personaSeedTests = testGroup "persona seed"
  [ testCase "quickstart search preserves query and decodes result" $
      assertEqual "typed search result"
        (Right [W.SearchHit "title" "https://example.org" "excerpt"])
        (run (interpret searchHost (W.search "query" 3 [] [])))
  , testCase "initialisation preserves an edited identity and unrelated notes" $ do
      let action = do
            _ <- M.initialise
            _ <- M.replaceBlock M.Persona "I have discovered a fondness for tiny maps."
            _ <- M.note "map.md" "A place to return to"
            M.initialise
          (_,files) = run (runState (Map.empty :: Map.Map Text Text) (interpret writeHost action))
      assertEqual "identity is not reseeded" (Just "I have discovered a fondness for tiny maps.") (Map.lookup ".exomonad/state/memory/persona.md" files)
      assertEqual "notes survive" (Just "A place to return to") (Map.lookup ".exomonad/state/notes/map.md" files)
  , testCase "multiple branches decode in authored order and ignore inactive input" $ do
      let form = F.branchesMany "doors"
            [F.option (V.text "a") (F.textInput "a" Nothing), F.option (V.text "b") (F.textInput "b" Nothing)]
          prepared = prepareForm form
          draft ids = object ["f0" .= (ids :: [Text]), "f1" .= ("first" :: Text), "f2" .= ("second" :: Text)]
      assertEqual "reordered submission" (Right ["first","second"]) (decodeSubmission prepared (draft ["o1","o0"]))
      assertEqual "no selection" (Right []) (decodeSubmission prepared (object ["f0" .= ([] :: [Text]), "f1" .= True]))
      assertBool "duplicates refused" (case decodeSubmission prepared (draft ["o0","o0"]) of Left _ -> True; _ -> False)
  , testCase "sliders refuse invalid authorship and out-of-range answers" $ do
      assertBool "bad bounds" (case F.slider "x" 1 0 0 of Left _ -> True; _ -> False)
      case F.slider "x" 0 10 4 of
        Left err -> fail (show err)
        Right form -> do
          let prepared = prepareForm form
          assertEqual "inclusive endpoint" (Right 10) (decodeSubmission prepared (object ["f0" .= (10 :: Double)]))
          assertBool "out of range" (case decodeSubmission prepared (object ["f0" .= (11 :: Double)]) of Left _ -> True; _ -> False)
  , testCase "literal windows retain source evidence and line addresses" $ do
      let page = W.Page "https://example.org" "source-hash" "first\nneedle\nlast"
      assertEqual "literal match" [W.Passage 2 2 "needle"] (W.findLiteral page "needle")
      assertEqual "window" (Right (W.Passage 2 3 "needle\nlast")) (W.readLines page 2 20)
      assertBool "invalid offset" (case W.readLines page 0 1 of Left _ -> True; _ -> False)
  , testCase "unavailable semantic filtering retains every passage as uncertain" $ do
      let page = W.Page "https://example.org" "source-hash" "first\nsecond\nthird"
          answer = run (interpret jevUnavailable (interpret reflectionUnavailable (W.select W.ExplicitOnly "something curious" 1 page)))
      case answer of
        Left err -> fail (show err)
        Right selection -> do
          assertEqual "original retained" page (W.original selection)
          assertEqual "no confident omissions" [] (W.omitted selection)
          assertEqual "every passage unresolved" 3 (length (W.uncertain selection))
  , testCase "passage size above page length keeps the whole page, including maxBound" $ do
      let page = W.Page "https://example.org" "source-hash" "first\nsecond"
          answer = run (interpret jevUnavailable (interpret reflectionUnavailable
            (W.select W.ExplicitOnly "curious" maxBound page)))
      case answer of
        Right selection -> assertEqual "one complete uncertain passage"
          [W.Passage 1 2 "first\nsecond"] (map fst (W.uncertain selection))
        Left err -> fail (show err)
  , testCase "recent conversation selection requests four turns and exposes unavailability" $ do
      let page = W.Page "https://example.org" "source-hash" "first"
          answer = run (interpret jevUnavailable (interpret reflectionFour
            (W.select W.RecentFour "curious" 1 page)))
      case answer of
        Right selection -> assertEqual "context failure remains visible"
          "[recent conversation unavailable: ReflectUnbound]" (W.contextUsed selection)
        Left err -> fail (show err)
  , testCase "companion and helper profiles enable composition without raw transport or commands" $ do
      let companion = toolEffectNames (Proxy @(Contract.SyncEffects Spec.PersonaEffects))
          helper = toolEffectNames (Proxy @(Contract.SyncEffects Spec.HelperEffects))
          companionBase = toolEffectNames (Proxy @Spec.PersonaEffects)
          helperBase = toolEffectNames (Proxy @Spec.HelperEffects)
      mapM_ (\effect -> assertBool ("companion: " ++ show effect) (effect `elem` companion))
        ["ContextReadWrite", "Actor", "AgentLaunch", "Replies", "Watches", "ResourceScopes", "FsWrite", "AskUser"]
      mapM_ (\effect -> assertBool ("helper: " ++ show effect) (effect `elem` helper))
        ["ContextReadWrite", "Actor", "AgentLaunch", "Replies", "Watches", "Web", "Jev"]
      mapM_ (\effect -> assertBool ("excluded: " ++ show effect) (effect `notElem` companion && effect `notElem` helper))
        ["Commands", "Http"]
      assertBool "default helper returns findings rather than writing shared memory or mounting human forms"
        ("FsWrite" `notElem` helper && "AskUser" `notElem` helper)
      mapM_ (\effect -> assertBool ("invocation-only: " ++ show effect)
        (effect `notElem` companionBase && effect `notElem` helperBase))
        ["ContextReadWrite", "Reload"]
      assertEqual "helper grant witnesses match its exact base profile" helperBase
        (map actorEffectName (effectKeys (knownEffects @Spec.HelperEffects)))
      Spec.agentSpec @ActorEffects `seq`
        Contract.installSpec @Spec.HelperEffects Spec.helperSpec `seq` pure ()
  , testCase "installed file tools use object inputs and precede the notebook" $ do
      let field name (Object fields) = Map.findWithDefault Null name fields
          field _ _ = Null
      case Contract.prepareAgentSpec (Spec.agentSpec @Spec.PersonaEffects) of
        Left problem -> fail (show problem)
        Right prepared -> case field "tools" (Contract.preparedAgentSpecMetadata prepared) of
          Array tools -> do
            assertEqual "explicit ordinary tool order"
              (map String ["read_file", "write_file", "edit_file", "preview_edit", "lookup", "readLookup", "haskell", "reload"])
              (map (field "name") tools)
            mapM_ (\tool -> assertEqual "hosted function input root" (String "object")
              (field "type" (field "inputSchema" tool)))
              [tool | tool <- tools, field "name" tool /= String "haskell"]
            case [field "inputSchema" tool | tool <- tools, field "name" tool == String "write_file"] of
              [schema] -> case field "oneOf" (field "operation" (field "properties" schema)) of
                Array variants -> assertEqual "typed create/replace intent is nested" 2 (length variants)
                other -> fail (show other)
              other -> fail (show other)
          other -> fail (show other)
  , testCase "startup seeds preserve authored order and distinguish absent from empty caller input" $ do
      let first = Contract.contextSection "first" (Just "memory/a.md") "A"
          second = Contract.contextSection "second" Nothing "B"
      assertEqual "left identity" first (mempty <> first)
      assertEqual "right identity" first (first <> mempty)
      assertEqual "ordered append" "[first; source: memory/a.md]\nA\n\n[second]\nB"
        (Contract.renderContextSeed (first <> second))
      assertEqual "empty seed" "" (Contract.renderContextSeed mempty)
      assertEqual "absent caller seed" (Success (Contract.StartupInput "operation" Nothing))
        (fromJSON (object ["operation" .= ("operation" :: Text)]))
      assertEqual "explicit empty caller seed" (Success (Contract.StartupInput "operation" (Just "")))
        (fromJSON (object ["operation" .= ("operation" :: Text), "callerSeed" .= ("" :: Text)]))
  ]
