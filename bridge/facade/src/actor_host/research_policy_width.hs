let wideGroup = "too-wide" :: ForkGroupLabel
let firstLabel = [label|first|]
let secondLabel = [label|second|]
widthResult <- attemptUnfold (subgroup wideGroup) ((,) <$> child (researching @Text currentCheckout (assignment firstLabel ())) <*> child (researching @Text currentCheckout (assignment secondLabel ())))
case widthResult of { Left _ -> True; Right _ -> False }
