-- Signed request-style helpers are checked with the original cell root.
-- The explicit signatures exercise GHC's mono/poly identity export pair.
replyToken :: Int
replyToken = 3

respond :: Int -> Eff '[] Int
respond = replyWith replyToken

replyWith :: Int -> Int -> Eff '[] Int
replyWith token value = pure (token + value)

recursiveEven :: Int -> Eff '[] Int
recursiveEven 0 = pure 5
recursiveEven value = recursiveOdd (value - 1)

recursiveOdd :: Int -> Eff '[] Int
recursiveOdd 0 = pure 7
recursiveOdd value = recursiveEven (value - 1)

helperStep :: forall value. Num value => value -> value
helperStep value = value + 1
