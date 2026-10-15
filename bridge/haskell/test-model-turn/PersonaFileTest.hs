{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
module PersonaFileTest (personaFileTests) where
import Prelude
import Control.Monad.Freer (Eff, interpret, run)
import Control.Monad.Freer.State (State, get, put, runState)
import Data.Text (Text)
import qualified Data.Text as T
import Data.Proxy (Proxy(..))
import Test.Tasty.HUnit (assertBool, assertEqual)
import Test.Tasty.QuickCheck (testProperty, (===), counterexample)
import Tidepool.Test.Runner (TestTree, testGroup, testCase)
import Tidepool.Aeson
import Tidepool.Aeson.Schema
import Tidepool.Effects.Core (FsRead(..), FsWrite(..), FsSnapshot(..), FsWriteReceipt(..), FsPublication(..))
import Tidepool.Records (FsError(..))
import qualified Tidepool.File as File
import qualified Persona.FileTools as Tools
import qualified Persona.Patch as Patch

-- Controlled filesystem effect owner. Real hashes/locks/publication are tested
-- independently by the native handler history suite; these unique fixture
-- versions isolate the authored adapter's choice of precondition and proposal.
data World = World { body :: Maybe Text, attempts :: Int, publications :: Int
                   , race :: Maybe (Maybe Text), durabilityUnknown :: Bool } deriving Show
initial :: Maybe Text -> World
initial text = World text 0 0 Nothing False
fixtureBodies :: [Text]
fixtureBodies = ["cat", "dog", "owl", "α\r\nβ\n", "cat cat", "aaa", "cat!", "new"]
digest :: Text -> Text
digest text = case lookup text (zip fixtureBodies ['1'..'8']) of
  Just c -> T.replicate 63 "0" <> T.singleton c
  Nothing -> error ("undeclared controlled body: " ++ show text)
reader :: FsRead a -> Eff '[FsWrite, State World] a
reader (FsReadSnapshot _) = do
  state <- get @World
  pure $ case body state of Nothing -> Left (FsNotFound "/file"); Just text -> Right (FsSnapshot "/file" text (digest text))
reader _ = error "file tools must read a single snapshot, not separate text/hash observations"
writer :: FsWrite a -> Eff '[State World] a
writer (FsWriteCas target expected proposal) = do
  before <- get @World
  let current = maybe (body before) id (race before)
      observed = digest <$> current
      state = before { body = current, race = Nothing, attempts = attempts before + 1 }
  if expected /= observed then put state >> pure (Right (Left observed))
  else do
    put state { body = Just proposal, publications = publications state + 1 }
    pure (Right (Right (FsWriteReceipt target (digest proposal)
      (if durabilityUnknown state then FsVisibleDurabilityUnconfirmed "controlled sync failure" else FsDurable))))
writer _ = error "file tools must use conditional publication, never unconditional writes"
execute :: World -> Eff '[FsRead, FsWrite, State World] a -> (a, World)
execute state program = run $ runState state $ interpret writer $ interpret reader program
reference :: Eff '[FsRead, FsWrite, State World] File.FileReference
reference = do
  result <- File.readSnapshot "/file"
  pure $ case result of Right snapshot -> File.snapshotReference snapshot; Left _ -> error "reference requires observed fixture"
external :: Maybe Text -> Eff '[FsRead, FsWrite, State World] ()
external text = do state <- get @World; put state { body = text }

personaFileTests :: TestTree
personaFileTests = testGroup "persona file tools"
  [ testCase "stale earlier read refuses even when edits match fresh contents" $ do
      let (result, after) = execute (initial (Just "cat")) $ do
            old <- reference
            external (Just "dog")
            Tools.editFileTool (Tools.EditInput old [Patch.Replace "dog" "owl"])
      assertBool "stale base refused before anchor application" (case result of Left (Tools.PatchRejected (Patch.VersionMismatch _ _)) -> True; _ -> False)
      assertEqual "fresh body untouched" (Just "dog") (body after)
      assertEqual "no conditional write was submitted" 0 (attempts after)
  , testCase "preview and edit share exact preparation and reject stale bases" $ do
      let ((preview, edited), after) = execute (initial (Just "cat")) $ do
            observed <- reference
            preview <- Tools.previewEditTool (Tools.EditInput observed [Patch.Replace "cat" "dog"])
            external (Just "owl")
            edited <- Tools.editFileTool (Tools.EditInput observed [Patch.Replace "cat" "dog"])
            pure (preview, edited)
      assertBool "preview succeeded" (case preview of Right _ -> True; _ -> False)
      assertBool "commit does not refresh preview base" (case edited of Left (Tools.PatchRejected (Patch.VersionMismatch _ _)) -> True; _ -> False)
      assertEqual "preview submitted no write" 0 (attempts after)
  , testCase "read ranges preserve CRLF and version the entire file" $ do
      let ((full, excerpt, changed), _) = execute (initial (Just "α\r\nβ\n")) $ do
            full <- Tools.readFileTool (Tools.ReadInput "/file" Nothing Nothing)
            observed <- reference
            excerpt <- Tools.readFileTool (Tools.ReadInput "/file" (Just (File.referenceVersion observed)) (Just (Tools.LineRange 2 2)))
            external (Just "cat")
            changed <- Tools.readFileTool (Tools.ReadInput "/file" (Just (File.referenceVersion observed)) (Just (Tools.LineRange 1 1)))
            pure (full, excerpt, changed)
      case (full, excerpt) of
        (Right (Tools.ReadReceipt whole _ _ _ _), Right (Tools.ReadReceipt selected text _ before after)) -> do
          assertEqual "whole-file version retained" whole selected
          assertEqual "no newline rewriting" "β\n" text
          assertBool "explicit omitted prefix" before
          assertBool "no omitted suffix" (not after)
        _ -> error "expected full and range reads"
      assertBool "later changed file cannot impersonate old range" (case changed of Left (Tools.ReadVersionMismatch _ _) -> True; _ -> False)
  , testCase "invalid ranges and missing reads stay explicit" $ do
      let ((invalid, missing), _) = execute (initial (Just "cat")) $ do
            invalid <- Tools.readFileTool (Tools.ReadInput "/file" Nothing (Just (Tools.LineRange 0 1)))
            external Nothing
            missing <- Tools.readFileTool (Tools.ReadInput "/file" Nothing Nothing)
            pure (invalid, missing)
      assertBool "invalid range" (case invalid of Left (Tools.InvalidRange _) -> True; _ -> False)
      assertBool "missing differs from empty contents" (case missing of Left (Tools.StorageFailure (FsNotFound _)) -> True; _ -> False)
  , testCase "change or delete after prepare conflicts without publication" $ do
      mapM_ (\replacement -> do
        let (result, after) = execute ((initial (Just "cat")) { race = Just replacement }) $ do
              observed <- reference
              Tools.editFileTool (Tools.EditInput observed [Patch.Replace "cat" "dog"])
        assertBool "CAS owns the second race" (case result of Right (File.Conflict _ _ _) -> True; _ -> False)
        assertEqual "competing state preserved" replacement (body after)
        assertEqual "one CAS attempt" 1 (attempts after)
        assertEqual "no publication" 0 (publications after)) [Nothing, Just "owl"]
  , testCase "create and replace are explicit with no unconditional fallback" $ do
      let ((created, duplicate, stale), after) = execute (initial Nothing) $ do
            created <- Tools.writeFileTool (Tools.Create "/file" "cat")
            observed <- reference
            duplicate <- Tools.writeFileTool (Tools.Create "/file" "dog")
            external Nothing
            stale <- Tools.writeFileTool (Tools.Replace observed "owl")
            pure (created, duplicate, stale)
      assertBool "create publication receipt" (case created of Right (File.Published _ File.Durable) -> True; _ -> False)
      assertBool "create cannot overwrite" (case duplicate of Right (File.Conflict _ Nothing (Just _)) -> True; _ -> False)
      assertBool "replace cannot recreate deletion" (case stale of Right (File.Conflict _ (Just _) Nothing) -> True; _ -> False)
      assertEqual "no forced recreation" Nothing (body after)
      assertEqual "only first publication" 1 (publications after)
  , testCase "ABA content identity deliberately permits restored bytes" $ do
      let (result, after) = execute (initial (Just "cat")) $ do
            observed <- reference; external (Just "owl"); external (Just "cat")
            Tools.editFileTool (Tools.EditInput observed [Patch.Replace "cat" "dog"])
      assertBool "same restored content is same version" (case result of Right (File.Published _ File.Durable) -> True; _ -> False)
      assertEqual "edit applied" (Just "dog") (body after)
  , testCase "anchor refusals and later failure submit zero writes" $ do
      mapM_ (\(source, edits) -> do
        let (result, after) = execute (initial (Just source)) $ do
              observed <- reference; Tools.editFileTool (Tools.EditInput observed edits)
        assertBool "typed patch refusal" (case result of Left (Tools.PatchRejected _) -> True; _ -> False)
        assertEqual "all hunks are atomic" (Just source) (body after)
        assertEqual "no write dispatch" 0 (attempts after))
        [("cat", []), ("cat", [Patch.Replace "" "dog"]), ("cat", [Patch.Replace "owl" "dog"])
        ,("cat cat", [Patch.Replace "cat" "dog"]), ("aaa", [Patch.Replace "aa" "dog"])
        ,("cat", [Patch.Replace "cat" "dog", Patch.Replace "owl" "cat"])]
      assertBool "existing pure checks" (all snd Patch.checks)
  , testCase "visible durability uncertainty is retained and never replayed" $ do
      let (result, after) = execute ((initial (Just "cat")) { durabilityUnknown = True }) $ do
            observed <- reference; Tools.editFileTool (Tools.EditInput observed [Patch.Replace "cat" "dog"])
      case result of
        Right (File.Published published (File.VisibleDurabilityUnconfirmed _)) ->
          assertEqual "known published identity" (digest "dog") (File.contentVersionText (File.referenceVersion published))
        _ -> error "visible publication was demoted to a retryable failure"
      assertEqual "one publication, no replay" 1 (publications after)
      assertEqual "one attempt" 1 (attempts after)
  , testCase "version codec and FsError schema match their public encodings" $ do
      let valid = T.replicate 64 "a"
      case fromJSON (String valid) :: Result File.ContentVersion of
        Success version -> assertEqual "exact round trip" (String valid) (toJSON version)
        Error err -> error err
      mapM_ (\invalid -> assertBool "invalid digest refused" (case fromJSON (String invalid) :: Result File.ContentVersion of Error _ -> True; _ -> False)) ["", T.replicate 64 "g", T.replicate 64 "A", T.replicate 63 "a"]
      let errors = [FsNotFound "p", FsNotUtf8 "p", FsSandbox "d", FsBadRegex "d", FsIo "d", FsDurabilityUnknown "d", FsNonUtf8Path "p"]
          names = ["FsNotFound", "FsNotUtf8", "FsSandbox", "FsBadRegex", "FsIo", "FsDurabilityUnknown", "FsNonUtf8Path"]
          fields = ["path", "path", "detail", "detail", "detail", "detail", "path"]
          expected = object [("oneOf", Array [objectSchema (Just name) [(field, jsonSchema (Proxy @Text), True)] | (name,field) <- zip names fields])]
      assertEqual "schema is exactly the tagged owner encoding" expected (jsonSchema (Proxy @FsError))
      mapM_ (\(err,name,field) -> assertEqual "tagged value" (object ["tag" .= (name :: Text), field .= (if field == "path" then "p" else "d" :: Text)]) (toJSON err)) (zip3 errors names fields)
  , testProperty "earlier-version edit follows independent content histories" $ \choices ->
      let values = take 24 (map (\n -> fixtureBodies !! (abs (n `mod` 3))) (choices :: [Int]))
          histories = [let (result, after) = execute (initial (Just "cat")) $ do
                            observed <- reference; mapM_ (external . Just) prefix
                            Tools.editFileTool (Tools.EditInput observed [Patch.Replace (last ("cat":prefix)) "new"])
                       in if last ("cat":prefix) == "cat"
                            then body after == Just "new" && publications after == 1
                            else body after == Just (last prefix) && attempts after == 0
                      | size <- [0..length values], let prefix = take size values]
      in counterexample (show values) (and histories === True)
  ]
