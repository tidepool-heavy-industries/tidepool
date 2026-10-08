data JevOperation = Prepare | Inspect | Map | Rejudge | Project | Execute | Run
data SemanticDecision = MissingInformation | Proceed deriving Eq
_ <- do
  let action = F.note "selected-action" >> pure (23 :: Int)
      packet quick careful = #route J.:= J.choice "Which path?"
        (J.alt #quick "Quick path" quick J..| J.alt #careful "Careful path" careful)
      prepared quick careful = case J.prepare J.jevLatest (J.rawState (object [])) (packet quick careful) of
        Right retained -> retained
        Left _ -> error "history packet did not prepare"
      retained = prepared action (error "unselected action demanded")
      wire winner other confidence usage = object
        ["answers" .= object ["route" .= object
          ["type" .= ("choice" :: Text), "choice" .= winner,
           "probabilities" .= object [winner .= (0.7 :: Double), other .= (0.3 :: Double)],
           "confidence" .= confidence]],
         "model" .= ("stub-1.0" :: Text), "usage" .= usage]
      recorded = wire ("quick" :: Text) ("careful" :: Text) (0.7 :: Double)
        (object ["input_tokens" .= (100 :: Int), "output_tokens" .= (7 :: Int)])
      decoded plan recording = case J.decode plan recording of
        Right response -> response
        Left _ -> error "native response decoder refused deterministic recording"
      initial = decoded retained recorded
      metadata response = (J.responsePreview response, J.resolvedModel response,
        J.rawUsage response, J.usage response, J.diagnostics response)
      require condition message = if condition then pure () else error message
      select response = case J.takenUnder J.careful (J.answers response).route of
        Right decision -> J.settledValue decision
        Left _ -> error "careful policy did not select scripted original"
      step response operation = case operation of
        Prepare -> require (J.request retained == J.request (prepared (pure 59) (pure 73)))
          "payload changed wire request" >> pure response
        Inspect -> do
          let mappedBottom = fmap (const (error "mapped payload inspected" :: Int)) response
              (preview, _) = displayWith 8192 mappedBottom
              fieldsAreEvidence = case displayTree mappedBottom of
                Constructor "Jev.Response" fields -> map fst fields == ["originalAnswerEvidence", "model", "usage", "diagnostics"]
                _ -> False
          require (metadata mappedBottom == metadata initial && T.length preview > 0 && fieldsAreEvidence)
            "inspection changed evidence or forced mapped payload"
          pure response
        Map -> do
          let identity = fmap id response
              base = fmap (const (11 :: Int)) response
              composed = fmap ((+ 3) . (* 2)) base
              separate = fmap (+ 3) (fmap (* 2) base)
          require (metadata identity == metadata initial && metadata composed == metadata separate
            && J.answers composed == 25 && J.answers separate == 25)
            "Response functor identity/composition failed observationally"
          pure response
        Rejudge -> do
          require (case J.takenUnder J.strict (J.answers response).route of Left _ -> True; Right _ -> False)
            "strict policy unexpectedly accepted low confidence"
          require (fst (displayWith 8192 (J.takenUnder J.careful (J.answers response).route)) /= "")
            "settled action could not be inspected opaquely"
          pure response
        Project -> let selected = select response in selected `seq` pure response
        Execute -> J.executePrepared retained >>= \result -> case result of
          Right fresh -> require (metadata fresh == metadata initial) "explicit execution changed evidence" >> pure fresh
          Left _ -> error "public executePrepared failed"
        Run -> select response >>= \value -> require (value == 23) "wrong action selected" >> pure response
      operations = [Prepare, Inspect, Map, Rejudge, Project, Execute, Run]
      histories = [[first, second] | first <- operations, second <- operations]
  forM_ (histories ++ [[Execute, Run, Run]]) (\history -> foldM step initial history >> pure ())
  let originalFunction = decoded (prepared ((+ 7) :: Int -> Int) (error "unselected function demanded")) recorded
      replacementFunction = decoded (prepared ((+ 31) :: Int -> Int) (error "unselected function demanded")) recorded
      selectedFunction response = case J.takenUnder J.careful (J.answers response).route of
        Right decision -> J.settledValue decision
        Left _ -> error "original function did not settle"
      unknown = decoded retained (wire ("quick" :: Text) "careful" (0.7 :: Double) Null)
      zero = decoded retained (wire ("quick" :: Text) "careful" (0.7 :: Double)
        (object ["input_tokens" .= (0 :: Int), "output_tokens" .= (0 :: Int)]))
      driftRecording = object
        ["answers" .= object ["route" .= object
          ["type" .= ("choice" :: Text), "choice" .= ("quick" :: Text),
           "probabilities" .= object ["quick" .= (0.7 :: Double), "careful" .= (0.2 :: Double)],
           "confidence" .= (0.9 :: Double)]],
         "model" .= ("drift-fixture" :: Text),
         "usage" .= object ["input_tokens" .= (0 :: Int)]]
      drifted = decoded retained driftRecording
      mappedDrift = fmap (const (error "mapped drift payload inspected" :: Int)) drifted
      missingPacket = #route J.:= J.choice "What does the input justify?"
        (J.alt #missing "No required delivery criteria were supplied" MissingInformation
          J..| J.alt #proceed "Required criteria were supplied" (error "unselected semantic payload demanded"))
      missingPrepared = case J.prepare J.jevLatest (J.rawState (object [])) missingPacket of
        Right value -> value
        Left _ -> error "missing information choice did not prepare"
      missingResponse = decoded missingPrepared (wire ("missing" :: Text) "proceed" (0.9 :: Double) Null)
  require (selectedFunction originalFunction 10 == 17 && selectedFunction replacementFunction 10 == 41)
    "identical requests lost original captured functions"
  require (J.usage unknown == J.Usage Nothing Nothing && J.usage zero == J.Usage (Just 0) (Just 0)
    && J.usage unknown /= J.usage zero) "unknown usage was treated as measured zero"
  require (metadata mappedDrift == metadata drifted
    && J.resolvedModel mappedDrift == "drift-fixture"
    && J.usage mappedDrift == J.Usage (Just 0) Nothing
    && (case J.diagnostics mappedDrift of
      [J.DistributionDrift question total] -> question == "route" && total > 0.89 && total < 0.91
      _ -> False)) "mapping dropped nonempty diagnostics or partially known usage"
  require (case J.takenUnder J.careful (J.answers missingResponse).route of
    Right decision -> J.settledValue decision == MissingInformation
    Left _ -> False) "confident missing-information selection became policy doubt"
  say "True"
