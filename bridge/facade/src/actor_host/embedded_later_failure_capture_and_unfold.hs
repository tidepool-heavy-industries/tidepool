do
  Right seed <- checkpoint "embedded parent checkpoint before later failure"
  _ <- unfold (batch ("embedded-later-failure" :: CampaignLabel)
    ("checkpoint-readers" :: ForkGroupLabel))
    ((,) <$>
      child (withContext (fromCheckpoint seed)
        (researching @Text projectHead
          (assignment [label|checkpoint-alpha|] ("read the captured Haskell context" :: Text))))
      <*> child (withContext (fromCheckpoint seed)
        (researching @Text projectHead
          (assignment [label|checkpoint-beta|] ("read the captured Haskell context" :: Text)))))
  pure True
