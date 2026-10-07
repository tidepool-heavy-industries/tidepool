_ <- do
  let action marker result = do
        F.note marker
        followup <- F.askUser (F.choice "Selected follow-up"
          (F.option (V.text "Continue") (pure result) :| []))
        case followup of
          F.Submitted original -> original
          _ -> error "selected follow-up did not submit"
      originalQuick = action "original-quick" (23 :: Int)
      originalCareful = action "unselected-original-careful" 41
      replacementQuick = action "unselected-replacement-quick" 59
      replacementCareful = action "unselected-replacement-careful" 73
      packet quick careful = #route J.:= J.choice "Which path?"
        (J.alt #quick (String "Quick path") quick J..| J.alt #careful (String "Careful path") careful)
      world = J.rawState (object [])
  original <- case J.prepare J.jevLatest world (packet originalQuick originalCareful) of
    Right prepared -> pure prepared
    Left _ -> error "original plan did not prepare"
  replacement <- case J.prepare J.jevLatest world (packet replacementQuick replacementCareful) of
    Right prepared -> pure prepared
    Left _ -> error "replacement plan did not prepare"
  _ <- if J.request original == J.request replacement then pure () else error "plans have different wire shapes"
  human <- F.askUser (F.choice "Prepared plan"
    (F.option (V.text "Original") original :| [F.option (V.text "Replacement") replacement]))
  prepared <- case human of
    F.Submitted retained -> pure retained
    _ -> error "prepared plan was not selected"
  reply <- Host.jevTransport (J.request prepared)
  envelope <- case reply of
    Right wire -> case J.decode prepared wire of
      Right decoded -> pure decoded
      Left _ -> error "prepared decoder rejected the scripted response"
    Left _ -> error "scripted Jev transport failed"
  let projected = J.mapResponse (const ()) envelope
      metadata = J.responseModel envelope == "stub-1.0"
        && J.usage envelope == J.Usage (Just 100) (Just 7)
        && J.diagnostics envelope == []
        && J.responseModel projected == J.responseModel envelope
        && J.rawUsage projected == J.rawUsage envelope
        && J.responsePreview projected == J.responsePreview envelope
        && J.diagnostics projected == J.diagnostics envelope
  selected <- case J.takenUnder J.careful (J.answers envelope).route of
    Right decision -> pure (J.settledValue decision)
    Left _ -> error "scripted choice did not settle under careful"
  result <- selected
  say (tshow (metadata && result == 23))
