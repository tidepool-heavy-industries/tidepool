let baseStage = (7 :: Int)
data MixedStage = MixedStage Int
class MixedStageReader value where
  readMixedStage :: value -> Int
instance MixedStageReader MixedStage where
  readMixedStage (MixedStage value) = value + baseStage
{-# NOINLINE dormantStage #-}
dormantStage :: Int
dormantStage = StageInputG.g
let answerStage = MixedStage baseStage
let () = ()
let resultStage = readMixedStage answerStage + dormantStage
