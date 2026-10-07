-- These definitions belong to the trusted template's original target.
replyToken :: Int
replyToken = 3

respond :: Int -> SegmentHelperEff.Eff __EFFECT_ROW__ Int
respond = replyWith replyToken

replyWith :: Int -> Int -> SegmentHelperEff.Eff __EFFECT_ROW__ Int
replyWith token value = pure (token + value)

recursiveEven :: Int -> SegmentHelperEff.Eff __EFFECT_ROW__ Int
recursiveEven 0 = pure 5
recursiveEven value = recursiveOdd (value - 1)

recursiveOdd :: Int -> SegmentHelperEff.Eff __EFFECT_ROW__ Int
recursiveOdd 0 = pure 7
recursiveOdd value = recursiveEven (value - 1)

helperStep :: forall value. Num value => value -> value
helperStep value = value + 1

-- Ordinary top-level function generalization; no export wrapper is inverted.
inferredStep value = value + 1

reportHelpers :: Int -> Int -> Int -> Double -> Int -> Double -> SegmentHelperEff.Eff __EFFECT_ROW__ ()
reportHelpers reply recursive integerStep doubleStep inferredInteger inferredDouble =
  if inferredInteger == 5 && inferredDouble == 5.0
    then SegmentHelperEff.send (Print (SegmentHelperText.pack
      ("[" ++ show reply ++ "," ++ show recursive ++ "," ++ show integerStep ++ "," ++ show doubleStep ++ "]")))
    else error "inferred template helper lost its independent instantiations"
