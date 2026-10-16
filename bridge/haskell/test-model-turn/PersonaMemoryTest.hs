{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
module PersonaMemoryTest (personaMemoryTests) where

import Prelude
import Control.Lens (view)
import Control.Exception (SomeException, evaluate, try)
import Control.Monad.Freer (Eff, Member, interpret, run)
import Control.Monad.Freer.State (State, get, modify, runState)
import qualified Data.Map.Strict as Map
import Data.Text (Text)
import qualified Data.Text as T
import Test.Tasty.HUnit (assertBool, assertEqual)
import Test.Tasty.QuickCheck (testProperty, (===))
import Tidepool.Test.Runner (TestTree, testGroup, testCase)
import Tidepool.Agent.Contract (StartupInput(..), ContextSeed(..), ContextSection(..))
import Tidepool.Effects.Core (FsRead(..), FsWrite(..), FsWriteReceipt(..), FsPublication(..), FsSnapshot(..), ContextReadWrite(..), ContextDocument(..), ContextReference(..), ContextInspection(..), ContextTextVersion(..), ContextUnitVersion(..))
import Tidepool.Records (FsError(..))
import qualified Tidepool.Agent.Context as C
import qualified Persona.Memory as Memory
import qualified Persona.Orientation as O
import qualified Persona.Context as Context

type Files = Map.Map Text (Either FsError Text)
paths :: [Text]
paths = map (".exomonad/state/memory/" <>) ["persona.md", "human.md", "curiosities.md"]
readHost :: Member (State Files) effects => FsRead a -> Eff effects a
readHost (FsRead file) = Map.findWithDefault (Left (FsNotFound file)) file <$> get @Files
readHost _ = error "unexpected file observation"
writeHost :: Member (State Files) effects => FsWrite a -> Eff effects a
writeHost (FsWriteCas file Nothing body) = do
  files <- get @Files
  if Map.member file files then pure (Right (Left (Just "existing")))
  else modify @Files (Map.insert file (Right body)) >> pure (Right (Right (FsWriteReceipt file "published-version" FsDurable)))
writeHost _ = error "startup must only create missing seeds"
render :: Files -> Either FsError Text
render files = fst $ run $ runState files $ interpret readHost O.renderMemory
startup :: Files -> (ContextSeed, Files)
startup files = run $ runState files $ interpret readHost $ interpret writeHost $
  O.startup (StartupInput "test-startup" Nothing)

observedContext :: ContextInspection -> C.Context
observedContext inspection = run (interpret owner C.getContext)
  where
    owner :: ContextReadWrite a -> Eff '[] a
    owner GetContextWith = pure inspection
    owner _ = error "read-only context observation"

personaMemoryTests :: TestTree
personaMemoryTests = testGroup "persona memory"
  [ testProperty "rendering retains every authored body verbatim in order" $ \a b c ->
      let bodies = map T.pack [a,b,c]
      in render (Map.fromList (zip paths (map Right bodies))) === Right (T.intercalate "\n\n" (zipWith (\p body -> "[Memory file: " <> p <> "]\n" <> body) paths bodies))
  , testCase "startup preserves existing corrections and creates only missing seeds" $ do
      let correction = "# Human\n\nCorrection: answer from the memory already supplied at arrival.\n"
          files = Map.fromList [(paths !! 1, Right correction), ("unrelated", Right "untouched")]
          (ContextSeed sections, after) = startup files
          (_, again) = startup after
      assertEqual "authored correction unchanged" (Just (Right correction)) (Map.lookup (paths !! 1) after)
      assertEqual "only missing core files created" (Map.keys (Map.union files (Map.fromList [(p, Right "") | p <- paths]))) (Map.keys after)
      assertEqual "repeated startup preserves all files" after again
      assertBool "complete correction supplied" (correction `T.isInfixOf` contextSectionBody (head sections))
  , testCase "empty memory is observed while missing and inaccessible memory fail" $ do
      let files = Map.fromList [(p, Right "") | p <- paths]
      assertEqual "empty is valid authored content" (Right (T.intercalate "\n\n" ["[Memory file: " <> p <> "]\n" | p <- paths])) (render files)
      assertEqual "missing remains missing" (Left (FsNotFound (head paths))) (render (Map.delete (head paths) files))
      assertEqual "access failure remains failure" (Left (FsIo "denied"))
        (render (Map.insert (head paths) (Left (FsIo "denied")) files))
  , testCase "startup fails rather than publishing inaccessible memory" $ do
      let (ContextSeed sections, _) = startup (Map.fromList [(head paths, Left (FsIo "denied"))])
      result <- try (evaluate (sum (map (T.length . contextSectionBody) sections))) :: IO (Either SomeException Int)
      assertBool "refused publication" (case result of Left _ -> True; Right _ -> False)
  , testCase "revision preserves snapshot storage errors and performs no write" $ do
      let reader :: FsRead a -> Eff '[FsWrite] a
          reader (FsReadSnapshot _) = pure (Left (FsIo "snapshot denied"))
          reader _ = error "revision must read contents and version together"
          writer :: FsWrite a -> Eff '[] a
          writer _ = error "failed snapshot must not write"
      assertEqual "typed acquisition failure"
        (Left (FsIo "snapshot denied"))
        (run (interpret writer (interpret reader (Memory.reviseAt "memory" (<> "new")))))
  , testCase "revision preserves visible durability uncertainty without retry" $ do
      let reader :: FsRead a -> Eff '[FsWrite, State Int] a
          reader (FsReadSnapshot file) = pure (Right (FsSnapshot file "old" "observed-version"))
          reader _ = error "revision must use the snapshot operation"
          writer :: FsWrite a -> Eff '[State Int] a
          writer (FsWriteCas file expected body)
            | file == "memory" && expected == Just "observed-version" && body == "oldnew" =
                modify @Int (+1) >> pure (Right (Right (FsWriteReceipt file "published-version" (FsVisibleDurabilityUnconfirmed "visible; sync unconfirmed"))))
          writer _ = error "revision changed its captured destination, version or proposal"
          (result, attempts) = run $ runState (0 :: Int) $
            interpret writer (interpret reader (Memory.reviseAt "memory" (<> "new")))
      assertEqual "visible publication is not a conflict or a successful retry"
        (Left (FsDurabilityUnknown "visible; sync unconfirmed"))
        result
      assertEqual "visible publication is never replayed" 1 attempts
  , testCase "unpublished startup context refuses explicit activation without edits" $ do
      let owner :: ContextReadWrite a -> Eff '[] a
          owner GetInitialContextWith = pure C.InitialContextAbsent
          owner _ = error "unpublished context must not be read or edited"
      assertEqual "owner observation preserved"
        (Left (Context.InitialContextUnavailable C.InitialContextAbsent))
        (run (interpret owner (Context.activateInitial "full authored body")))
  , testCase "refresh reads full memory but never initialises or resurrects unpublished context" $ do
      let files = Map.fromList [(p, Right p) | p <- paths]
          owner :: ContextReadWrite a -> Eff '[State Files] a
          owner GetInitialContextWith = pure C.InitialContextAbsent
          owner _ = error "unpublished context must not be edited"
          (result, after) = run $ runState files $ interpret owner $ interpret readHost Context.refresh
      assertEqual "explicit activation refused" (Left (Context.ActivationFailed (Context.InitialContextUnavailable C.InitialContextAbsent))) result
      assertEqual "no filesystem writes" files after
  , testCase "refresh refuses missing memory before touching context" $ do
      let owner :: ContextReadWrite a -> Eff '[State Files] a
          owner _ = error "failed memory read must not activate context"
          (result, after) = run $ runState (Map.empty :: Files) $ interpret owner $ interpret readHost Context.refresh
      assertEqual "missing remains a read failure" (Left (Context.MemoryReadFailed (FsNotFound (head paths)))) result
      assertEqual "refresh did not create seeds" Map.empty after
  , testCase "preview removes one issued note while preserving equal provider text" $ do
      let note = ContextReference "note"
          provider = ContextReference "provider"
          plain = ContextReference "plain"
          context = observedContext (ContextInspection (ContextDocument
            [ C.Text (Just plain) C.User "same" []
            , C.Native (ContextReference "group") C.Opaque "original preview" False
                [ C.ContextVisibleText note (C.MessageText 0) "same" True True
                , C.ContextVisibleText provider (C.MessageText 0) "same" True False
                ]
            ]) [] [ContextUnitVersion note "issued-note-version"] [])
      case C.unitTargets context of
        [target] -> case C.previewContext context (C.contextPatch [C.removeUnit target]) of
          Left err -> error (show err)
          Right preview -> assertEqual "only selected occurrence removed"
            [C.Text (Just plain) C.User "same" [],
             C.Native (ContextReference "group") C.Opaque "original preview" False
               [C.ContextVisibleText provider (C.MessageText 0) "same" True False]]
            (blocks (C.previewDocument preview))
        _ -> error "owner-issued removable note missing"
      assertEqual "provider text has no removal target" [note]
        (map C.unitReference (C.unitTargets context))
  , testCase "preview rejects group removal overlapping a nested text edit" $ do
      let group = ContextReference "exchange"
          child = ContextReference "message"
          context = observedContext (ContextInspection (ContextDocument
            [C.Native group C.CompletedExchange "preview" False
              [C.ContextVisibleText child (C.MessageText 0) "original" True False]])
            [ContextTextVersion child (C.MessageText 0) "issued-text-version"]
            [ContextUnitVersion group "issued-group-version"] [])
      case (C.unitTargets context, C.textTargets context) of
        ([unit], [text]) -> do
          let removal = C.removeUnit unit
              replacement = C.replaceText text "updated"
              codes result = either (map C.contextProblemCode) (const []) result
          assertEqual "each change independently previews" [[], []]
            (map (codes . C.previewContext context . C.contextPatch . (: [])) [removal, replacement])
          assertEqual "remove then edit is invalid" [C.ContextProblemInvalidPatch]
            (codes (C.previewContext context (C.contextPatch [removal, replacement])))
          assertEqual "edit then remove is invalid" [C.ContextProblemInvalidPatch]
            (codes (C.previewContext context (C.contextPatch [replacement, removal])))
        _ -> error "test observation must issue both exact targets"
  , testCase "preview preserves independent selectors and rejects stale content versions" $ do
      let reference = ContextReference "multipart"
          document = ContextDocument [C.Native (ContextReference "exchange") C.CompletedExchange "preview" False
            [C.ContextVisibleText reference (C.MessageText 0) "first" True False,
             C.ContextVisibleText reference (C.MessageText 1) "second" True False]]
          context = observedContext (ContextInspection document
            [ContextTextVersion reference (C.MessageText 0) "version-first",
             ContextTextVersion reference (C.MessageText 1) "version-second"] [] [])
          newer = observedContext (ContextInspection document
            [ContextTextVersion reference (C.MessageText 0) "version-newer",
             ContextTextVersion reference (C.MessageText 1) "version-second"] [] [])
      case C.textTargets context of
        [first, second] -> do
          let patch = C.contextPatch [C.replaceText first "new first", C.replaceText second "new second"]
          case C.previewContext context patch of
            Left err -> error (show err)
            Right preview -> assertEqual "distinct selectors both update"
              [C.Native (ContextReference "exchange") C.CompletedExchange "preview" False
                [C.ContextVisibleText reference (C.MessageText 0) "new first" True False,
                 C.ContextVisibleText reference (C.MessageText 1) "new second" True False]]
              (blocks (C.previewDocument preview))
          assertEqual "new inspection refuses old version" [C.ContextProblemStale]
            (either (map C.contextProblemCode) (const []) (C.previewContext newer patch))
        _ -> error "multipart fields lost their selectors"
  , testCase "authorship remains exact observation data through a pure preview" $ do
      let reference = ContextReference "assistant-message"
          selector = C.MessageText 0
          author = C.ContextTextAuthorship reference selector C.ContextAuthorAssistant False
          document = ContextDocument [C.Text (Just reference) C.Assistant "original" []]
          context = observedContext (ContextInspection document
            [ContextTextVersion reference selector "issued-text-version"] [] [author])
      assertEqual "inspection retains exact metadata" [author] (view C.contextAuthorship context)
      case C.textTargets context of
        [target] -> case C.previewContext context (C.contextPatch
          [C.replaceText target "updated", C.appendNote "new assistant note" [reference]]) of
          Left err -> error (show err)
          Right preview -> do
            assertEqual "preview retains original observation" [author] (C.previewInspectedAuthorship preview)
            assertEqual "new note has no invented issued identity"
              [C.Text (Just reference) C.Assistant "updated" [], C.Text Nothing C.Assistant "new assistant note" [reference]]
              (blocks (C.previewDocument preview))
            assertEqual "preview does not mutate inspection metadata" [author] (view C.contextAuthorship context)
        _ -> error "assistant target missing from test observation"
  ]
