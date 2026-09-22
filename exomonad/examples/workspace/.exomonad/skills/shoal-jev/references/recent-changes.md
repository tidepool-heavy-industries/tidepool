# Select a prepared command continuation

This helper reads recent commit subjects, asks which might explain a task, and
fetches the selected commit's stat without another model turn. Selection is a
reading aid, not a conclusion about the code. The task is an argument so intent
travels with the evidence. Every command argument comes from code or Git output.

```haskell
import Tidepool.Effects.Core (Jev, Commands)
inspectRecentChanges :: (Member Jev effects, Member Commands effects) => Text -> Eff effects Text
inspectRecentChanges task = do
      listed <- Cmd.quiet (Cmd.run (Cmd.argv ["git", "log", "-8", "--format=%H%x09%s"]))
      case Cmd.stdout listed of
        Left issue -> pure ("Cannot read history: " <> T.pack (show issue))
        Right history -> do
          let rows = map (T.breakOn "\t") (T.lines history)
              offers = J.alt #unresolved "No listed subject explains the task, or subjects lack the deciding detail" ()
                J..| J.many #commit fst (T.drop 1 . snd) rows
          answer <- J.ask1 (J.rawState (object ["task" .= (task :: Text), "recent_history" .= history]))
            (J.choice "Which listed commit subject identifies a change worth inspecting for `task`?" offers)
          case answer of
            Left err -> pure ("Jev unavailable: " <> T.pack (show err))
            Right a -> case J.settle J.lenient a
                 (#unresolved (\() -> pure "The listed subjects do not resolve what to read; inspect broader history or source.")
                   J..| #commit (\_ (oid, _) -> do
                     result <- Cmd.quiet (Cmd.run (Cmd.argv ["git", "show", "--stat", "--oneline", oid]))
                     pure (either (\issue -> "Cannot read selected commit: " <> T.pack (show issue)) id (Cmd.stdout result)))) of
              Left _ -> pure ("Needs inspection: " <> J.explain J.lenient a)
              Right (J.Settled act) -> act
```

Call `inspectRecentChanges "Which recent change could explain the command output regression?"`
with your actual question. The helper is defined by the cell, not a shipped API.
Keep the returned evidence if another judgment needs it. For several independent
questions over one state, use `J.ask` with a packet and read fields from `J.answers`.
`J.settle` checks the winner's distribution and dispatches through the handler its
label names; its doubt and the transport failure are separate cases. A confident
unresolved answer remains unresolved. `J` is present where the workspace pins the
Jev library. Use `shoal-jev` for per-row batteries, speculative questions, and
other patterns.
