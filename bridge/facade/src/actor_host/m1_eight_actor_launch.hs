_ <- do
  Right seed <- checkpoint "eight measured captured actors"
  (a, b, c, d, e, f, g, h) <- unfold (batch ("resident-measurement" :: CampaignLabel) ("eight-actors" :: ForkGroupLabel))
    ((,,,,,,,) <$>
      child (withContext (fromCheckpoint seed) (researching @Int projectHead (assignment [label|measure-a|] ("Run the bounded offline workload then respond 42." :: Text))))
      <*> child (withContext (fromCheckpoint seed) (researching @Int projectHead (assignment [label|measure-b|] ("Run the bounded offline workload then respond 42." :: Text))))
      <*> child (withContext (fromCheckpoint seed) (researching @Int projectHead (assignment [label|measure-c|] ("Run the bounded offline workload then respond 42." :: Text))))
      <*> child (withContext (fromCheckpoint seed) (researching @Int projectHead (assignment [label|measure-d|] ("Run the bounded offline workload then respond 42." :: Text))))
      <*> child (withContext (fromCheckpoint seed) (researching @Int projectHead (assignment [label|measure-e|] ("Run the bounded offline workload then respond 42." :: Text))))
      <*> child (withContext (fromCheckpoint seed) (researching @Int projectHead (assignment [label|measure-f|] ("Run the bounded offline workload then respond 42." :: Text))))
      <*> child (withContext (fromCheckpoint seed) (researching @Int projectHead (assignment [label|measure-g|] ("Run the bounded offline workload then respond 42." :: Text))))
      <*> child (withContext (fromCheckpoint seed) (researching @Int projectHead (assignment [label|measure-h|] ("Run the bounded offline workload then respond 42." :: Text)))))
  Right () <- releaseCheckpoint seed
  replies <- watch "eight measured replies" ((,,,,,,,) <$> awaitValue a <*> awaitValue b <*> awaitValue c <*> awaitValue d <*> awaitValue e <*> awaitValue f <*> awaitValue g <*> awaitValue h)
  Right (42, 42, 42, 42, 42, 42, 42, 42) <- awaitWatch replies
  display True >> pure ()
