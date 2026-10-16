import qualified Tidepool.Form as F
import qualified Tidepool.View as V

let evidence = [True, False]
F.note "I will ask two related questions."
_ <- display (V.column [V.markdown "Review the current evidence.", V.inspect evidence])
scopeAnswer <- F.askUser $
  (\() scope -> scope) <$> F.present (V.markdown "Which part should I check?")
    <*> F.textInput "Scope" Nothing
case scopeAnswer of
  F.Submitted scope -> do
    reasonAnswer <- F.askUser $
      (\() reason -> (scope, reason))
        <$> F.present (V.column [V.markdown "Why this part?", V.text scope])
        <*> F.textInput "Reason" Nothing
    case reasonAnswer of
      F.Submitted (chosenScope, reason) -> do
        _ <- display (V.column [V.markdown "Recorded", V.text chosenScope, V.text reason])
        pure ()
      F.Dismissed -> do
        _ <- display (V.text "The second form was dismissed.")
        pure ()
      F.FormUnavailable _ -> do
        _ <- display (V.text "The second form is unavailable.")
        pure ()
  F.Dismissed -> do
    _ <- display (V.text "The first form was dismissed.")
    pure ()
  F.FormUnavailable _ -> do
    _ <- display (V.text "The first form is unavailable.")
    pure ()
