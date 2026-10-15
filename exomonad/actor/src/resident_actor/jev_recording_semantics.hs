_ <- do
  let require condition message = if condition then pure () else error message
      exact = Number (scientific 123456789012345678901234567890123456789 (-97))
      raw = object ["雪" .= exact, "nested" .= [Null, Bool True, String "\nλ"]]
      nearby = object ["雪" .= Number (scientific 123456789012345678901234567890123456790 (-97)),
        "nested" .= [Null, Bool True, String "\nλ"]]
      causes = [J.JevUnconfigured, J.JevCallCap, J.JevTransport "断線", J.JevTimeout,
        J.JevHttp 429 "後で", J.JevCircuitOpen 503 987, J.JevClientSetup "設定",
        J.JevBodyLimit, J.JevMalformed "invalid"]
      occurrence = J.Occurrence 7 ["outer", "内"] ["route"]
      records = J.Recording raw [occurrence] (Right raw) :
        [J.Recording raw [occurrence] (Left cause) | cause <- causes]
      archive = Recording.recordingArchive records
      decoded = Recording.decodeArchive (Recording.encodeArchive archive)
  require (decoded == Right archive) "native JSON archive roundtrip lost exact observations"
  let literal = "{\"version\":1,\"records\":[{\"request\":{\"雪\":123456789012345678901234567890123456789e-97},\"occurrences\":[],\"outcome\":{\"Right\":123456789012345678901234567890123456789e-97}}]}"
      expected = Recording.recordingArchive [J.Recording (object ["雪" .= exact]) [] (Right exact)]
  require (Recording.decodeArchive literal == Right expected) "independent exact decimal and Unicode text did not decode"
  require (case Recording.decodeArchive "{" of Left (Recording.InvalidJSON _) -> True; _ -> False)
    "malformed JSON was accepted"
  require (Recording.decodeArchive "{\"version\":2,\"records\":[]}" == Left (Recording.UnsupportedVersion 2))
    "unknown archive version was accepted"
  require (case Recording.decodeArchive "{\"version\":1,\"records\":[{\"request\":null,\"occurrences\":[],\"outcome\":{\"Right\":null}},{\"request\":null}]}" of
      Left (Recording.MalformedRecord 1 _) -> True; _ -> False)
    "malformed suffix silently returned a decoded prefix"
  let world evidence = J.rawState (object ["evidence" .= evidence])
      query evidence wording quick = J.question (world evidence) #route
        (J.choice wording (J.alt #quick "Quick path" quick J..| J.alt #careful "Careful path" (error "unselected payload demanded" :: Int)))
      ready model evidence wording quick = case J.prepareQuery model (query evidence wording quick) of
        Right prepared -> prepared
        Left _ -> error "recording test query did not prepare"
      original = ready "named-model" raw "Which path?" (error "old payload demanded" :: Int)
      group = head (J.queryGroups original)
      response = object ["answers" .= object ["0.route" .= object
        ["type" .= ("choice" :: Text), "choice" .= ("quick" :: Text),
         "probabilities" .= object ["quick" .= (0.7 :: Double), "careful" .= (0.3 :: Double)],
         "confidence" .= (0.7 :: Double)]], "model" .= ("fixture" :: Text), "usage" .= Null]
      replayArchive = Recording.recordingArchive [J.Recording (J.groupRequest group) (J.groupOccurrences group) (Right response)]
      recovered = case Recording.decodeArchive (Recording.encodeArchive replayArchive) of
        Right retained -> Recording.archiveRecords retained
        Left _ -> error "native replay archive failed to decode"
      replay prepared = J.result (J.replayQuery prepared recovered)
      payloadOnly = ready "named-model" raw "Which path?" (31 :: Int)
  case replay payloadOnly of
    Right answer -> require (J.taken answer == 31) "payload-only edit did not replay with current payload"
    Left _ -> error "same exact request refused archived response"
  let actionQuery = J.question (world raw) #route (J.choice "Which path?"
        (J.alt #quick "Quick path" (F.note "archived-action-must-stay-inert" >> pure 61 :: Eff '[Console] Int)
          J..| J.alt #careful "Careful path" (error "unselected action demanded" :: Eff '[Console] Int)))
  case J.prepareQuery "named-model" actionQuery of
    Right prepared -> case J.result (J.replayQuery prepared recovered) of
      Right answer -> J.taken answer `seq` pure ()
      Left _ -> error "archive could not replay with a retained effectful payload"
    Left _ -> error "effectful payload query did not prepare"
  forM_ [ready "other-model" raw "Which path?" 31,
         ready "named-model" nearby "Which path?" 31,
         ready "named-model" (String "different evidence") "Which path?" 31,
         ready "named-model" raw "Different wording?" 31] $ \changed ->
    require (case replay changed of Left (J.ReplayFailed (J.ChangedRequest 0)) -> True; _ -> False)
      "changed wire request reused incompatible observation"
  let failed = Recording.recordingArchive [J.Recording (J.groupRequest group) (J.groupOccurrences group) (Left (J.JevCircuitOpen 503 987))]
  case Recording.decodeArchive (Recording.encodeArchive failed) of
    Right retained -> require
      (case J.result (J.replayQuery payloadOnly (Recording.archiveRecords retained)) of
        Left (J.CallFailed (J.Transport (J.JevCircuitOpen 503 987))) -> True; _ -> False)
      "typed host failure was reclassified during replay"
    Left _ -> error "typed failure archive did not decode"
  pure ()
pure True
