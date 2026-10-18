_ <- do
  result <- Workflow.dialogue
  evaluation <- case Workflow.retainedResponse result of
    Just original -> pure original
    Nothing -> error "authored dialogue lost retained successful response"
  response <- case J.result evaluation of
    Right packet -> pure packet
    Left _ -> error "authored dialogue retained a failed evaluation"
  envelope <- case J.observations evaluation of
    [observation] -> case J.observationResult observation of
      Right original -> pure original
      Left _ -> error "authored dialogue retained a failed observation"
    _ -> error "authored dialogue lost its single response observation"
  let chosen = Workflow.chooseRouteUnder J.careful response
      inspected = displayWith 8192 evaluation
      metadata = J.resolvedModel envelope == "authored-fixture"
        && J.usage envelope == J.Usage (Just 0) (Just 0)
        && J.diagnostics envelope == []
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
