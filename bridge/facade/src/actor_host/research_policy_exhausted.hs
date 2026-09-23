exhausted <- attemptUnfold (subgroup nestedGroup) (child (researching @Text currentCheckout (assignment nestedLabel ())))
case exhausted of { Left (UnfoldBeginRejected _) -> True; _ -> False }
