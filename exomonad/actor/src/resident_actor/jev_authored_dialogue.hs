_ <- do
  result <- Workflow.dialogue
  response <- case Workflow.retainedResponse result of
    Just original -> pure original
    Nothing -> error "authored dialogue lost retained successful response"
  let chosen = Workflow.chooseRouteUnder J.careful response
      inspected = displayWith 8192 response
      metadata = J.resolvedModel response == "authored-fixture"
        && J.usage response == J.Usage (Just 0) (Just 0)
        && J.diagnostics response == []
      matched = case Workflow.outcome result of
        Workflow.Finished Workflow.JevSelected (Workflow.Delivery Workflow.Express)
          (Workflow.DeliveryPrepared Workflow.Express "A delivery" "Retained note") ->
            case chosen of Right row -> Workflow.meaning row == Workflow.Delivery Workflow.Express; Left _ -> False
        Workflow.Finished (Workflow.HumanAfterPolicyDoubt doubt) (Workflow.Delivery Workflow.Express)
          (Workflow.DeliveryPrepared Workflow.Express "A delivery" "Retained note") ->
            case chosen of Left later -> J.cause doubt == J.cause later; Right _ -> False
        Workflow.Finished Workflow.JevSelected Workflow.MissingDeliveryCriteria
          (Workflow.CriteriaSupplied "A delivery" "Tomorrow, budget 8") ->
            case chosen of Right row -> Workflow.meaning row == Workflow.MissingDeliveryCriteria; Left _ -> False
        _ -> False
  -- Reading metadata, displaying, and judging this retained value again must
  -- neither issue Jev requests nor sequence another captured form action.
  say (tshow (metadata && matched && T.length (fst inspected) > 0))
