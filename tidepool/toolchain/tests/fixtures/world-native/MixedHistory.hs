let baseStage = (7 :: Int)
data MixedStage = MixedStage Int
class MixedStageReader value where
  readMixedStage :: value -> Int
instance MixedStageReader MixedStage where
  readMixedStage (MixedStage value) = value + baseStage
let answerStage = MixedStage baseStage
let resultStage = readMixedStage answerStage
