let nestedGroup = "descendants-nested" :: ForkGroupLabel
let grandchildLabel = [label|descendants-grandchild|]
grandchildResponse <- unfoldDeferred (subgroup nestedGroup) (child (researching @Int currentCheckout (assignment grandchildLabel (2 :: Int))))
