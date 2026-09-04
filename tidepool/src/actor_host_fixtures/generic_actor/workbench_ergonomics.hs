:info DefinitelyMissingFromShoal
:type request
:status
let rightValue (Right value) = value
let warningValue = rightValue (Right 7 :: Either Text Int)
warningValue
[fmt|value={warningValue:d}|]
