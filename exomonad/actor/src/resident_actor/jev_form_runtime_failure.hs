_ <- do
  let action :: Text -> Eff '[Console, AskUser, Jev] ()
      action marker = F.note marker
      packet = #route J.:= J.choice "Which path?"
        (J.alt #quick "Quick path" (action "unselected-quick") J..| J.alt #careful "Careful path" (action "unselected-careful"))
  prepared <- case J.prepare J.jevLatest (J.rawState (object [])) packet of
    Right retained -> pure retained
    Left _ -> error "failure control did not prepare"
  human <- F.askUser (F.choice "Prepared plan" (F.option (V.text "Original") prepared :| []))
  retained <- case human of
    F.Submitted original -> pure original
    _ -> error "failure control plan was not selected"
  reply <- J.executePrepared retained
  case reply of
    Left (J.Transport (Tidepool.Effects.Core.JevCircuitOpen 503 987)) -> do
      F.note "typed-circuit-open"
      say "True"
    Right _ -> error "transport refusal unexpectedly returned an answer"
    _ -> error "transport refusal did not retain status and retry delay"
