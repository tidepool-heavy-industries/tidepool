do
  let { watcherSpec = R.definition "dynamic-source-watcher" (Actor.Selected (knownEffects @'[Actor]))
      (Watcher
        0
        (\target -> do
          own <- R.self @Watcher
          R.attach (watcherEvent own) (R.lifecycle target))
        (\() -> R.get)
        (R.on mempty (\_ -> R.modify' (+ 1))))
    ; tools current = Tools
      { runCase = finishTool "Attach a lifecycle source to this actor." $ \_ -> do
          watcher <- R.start watcherSpec
          attached <- R.call (watcherBegin (R.client watcher)) watcher
          pure (CaseOutput (attached == Right ())
            (T.pack (show attached)) 0, current)
      }
    }
  serveToolsWith 0 tools
