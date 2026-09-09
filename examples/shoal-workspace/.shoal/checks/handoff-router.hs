import qualified Tidepool.Actor as Actor
import qualified Data.Text as Text
data Handoff result = Partial Text Text result | Final Text Text result | Ignore result | HandoffSnapshot (([(Text, Text)], [(Text, Text)]) -> result)
data WakeCount result = WakeCount Int (Int -> result)
let countDefinition = (Actor.stateful "handoff-wakes" Actor.ReadOnly (\n (WakeCount delta answer) -> pure (answer n, n + delta)) :: Actor.ActorDefinition Int WakeCount Int)
wakes <- Actor.startActor countDefinition 0
let progress lane observation = case observation of
      ProgressUpdate _ ref -> Partial lane ref ()
      ProgressClosed -> Ignore ()
      ProgressPending -> Ignore ()
      ProgressRejected failure -> error (Text.pack (show failure))
let final = (\lane result -> case result of
      Right receipt -> Final lane (responseValue receipt) ()
      Left failure -> error (Text.pack (show failure))) :: Text -> Either ResponseFailure (ResponseResult Text) -> Handoff ()
let sources = [Actor.progressSource leftProgress (progress "left"), Actor.progressSource rightProgress (progress "right"), Actor.settlementSource (forkedResponse left) (final "left"), Actor.settlementSource (forkedResponse right) (final "right")]
let routerDefinition = Actor.withSources sources (Actor.stateful "handoff" Actor.ReadOnly (\state@(partials, finals) message -> case message of
      Partial lane ref answer -> pure (answer, ((lane, ref) : partials, finals))
      Final lane ref answer -> do { Actor.cast wakes (WakeCount 1 (const ())); pure (answer, (partials, (lane, ref) : finals)) }
      Ignore answer -> pure (answer, state)
      HandoffSnapshot answer -> pure (answer (reverse partials, reverse finals), state)) :: Actor.ActorDefinition ([(Text, Text)], [(Text, Text)]) Handoff ([(Text, Text)], [(Text, Text)]))
handoff <- Actor.startActor routerDefinition ([], [])
