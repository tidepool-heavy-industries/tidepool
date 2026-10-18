import qualified Tidepool.Agent.Contract as A
import qualified Exomonad.Models as Models
import qualified Tidepool.Model.Selection as Selection

let fixtureOpenAI key options = Selection.openAI Models.catalogIdentity key options
Right captured <- checkpoint currentModel "route workers"
Right producerActor <- spawnSubagent (ForkCtx captured) (ForkWorktree projectHead)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "producer" })
Right consumerActor <- spawnSubagent (ForkCtx captured) (ForkWorktree projectHead)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "consumer" })
Right producer <- request @Text producerActor ("candidate" :: Text) defaultRequestOptions
Right consumer <- request @Text consumerActor ("reviewer ready" :: Text) defaultRequestOptions
forwarding <- route (settlement producer) (\settled -> case settled of { Right answer -> do { Right _ <- request @Text consumerActor answer defaultRequestOptions; pure () }; Left failure -> error (T.pack (show failure)) })
broken <- route (settlement producer) (\_ -> error "deliberate route failure")
reviewLaunch <- route (settlement producer) (\settled -> case settled of
  Right answer -> do
    Right reviewer <- spawnSubagent (FreshCtx answer) (ForkWorktree projectHead)
      ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies])) { spawnModel = fixtureOpenAI "sol" (Selection.OpenAIOptions Selection.OpenAILow), spawnLabel = Just "review" })
    Right _ <- request @Text reviewer answer defaultRequestOptions
    pure ()
  Left _ -> pure ())
