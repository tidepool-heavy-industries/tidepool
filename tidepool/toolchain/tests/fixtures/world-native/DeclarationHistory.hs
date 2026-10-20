data OriginalStage = OriginalStage Int
class OriginalStageReader value where
  readOriginalStage :: value -> Int
instance OriginalStageReader OriginalStage where
  readOriginalStage (OriginalStage value) = value
