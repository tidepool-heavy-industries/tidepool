do
  let { watcherSpec = R.definition "dynamic-source-watcher" (Actor.Selected (knownEffects @'[Actor]))
      (Watcher
        0
        (\target -> do
          own <- R.self @Watcher
          R.attach (watcherEvent own) (R.lifecycle target))
        (\() -> R.get)
        (R.on mempty (\_ -> R.modify' (+ 1))))
    ; replacementSpec = R.definition "dynamic-source-watcher-replacement" (Actor.Selected (knownEffects @'[Actor]))
      (Watcher
        0
        (\_ -> pure (Right ()))
        (\() -> R.get)
        (R.on mempty (\_ -> R.modify' (+ 10))))
    ; tools current = Tools
      { runCase = finishTool "Attach a lifecycle source to this actor." $ \_ -> do
          watcher <- R.start watcherSpec
          attached <- R.call (watcherBegin (R.client watcher)) watcher
          before <- R.call (watcherCount (R.client watcher)) ()
          successor <- R.replace watcher replacementSpec
          observed <- R.call (watcherCount (R.client successor)) ()
          _ <- R.finish successor
          pure (CaseOutput (attached == Right () && before == 1 && observed == 11)
            (T.pack (show attached)) observed, current)
      }
    }
  serveToolsWith 0 tools
