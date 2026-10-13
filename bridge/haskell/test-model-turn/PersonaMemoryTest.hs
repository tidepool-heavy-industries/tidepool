{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
module PersonaMemoryTest (personaMemoryTests) where

import Prelude
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
import Tidepool.Effects.Core (FsRead(..), FsWrite(..), ContextReadWrite(..))
import Tidepool.Records (FsError(..))
import qualified Tidepool.Agent.Context as C
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
  else modify @Files (Map.insert file (Right body)) >> pure (Right (Right ()))
writeHost _ = error "startup must only create missing seeds"
render :: Files -> Either FsError Text
render files = fst $ run $ runState files $ interpret readHost O.renderMemory
startup :: Files -> (ContextSeed, Files)
startup files = run $ runState files $ interpret readHost $ interpret writeHost $
  O.startup (StartupInput "test-startup" Nothing)

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
  ]
