_ <- do
  let action marker = F.note marker
      packet = #route J.:= J.choice "Which path?"
        (J.alt #quick (String "Quick path") (action "unselected-quick") J..| J.alt #careful (String "Careful path") (action "unselected-careful"))
  prepared <- case J.prepare J.jevLatest (J.rawState (object [])) packet of
    Right retained -> pure retained
    Left _ -> error "failure control did not prepare"
  human <- F.askUser (F.choice "Prepared plan" (F.option (V.text "Original") prepared :| []))
  retained <- case human of
    F.Submitted original -> pure original
    _ -> error "failure control plan was not selected"
  reply <- Host.jevTransport (J.request retained)
  case reply of
    Left (Tidepool.Effects.Core.JevCircuitOpen 503 987) -> do
      F.note "typed-circuit-open"
      say "True"
    Right wire -> case J.decode retained wire of
      Right envelope -> case J.takenUnder J.careful (J.answers envelope).route of
        Right decision -> J.settledValue decision >> error "transport refusal unexpectedly ran an action"
        Left _ -> error "transport refusal unexpectedly returned an unsettled answer"
      Left _ -> error "transport refusal unexpectedly returned a malformed answer"
    _ -> error "transport refusal did not retain status and retry delay"
