leafObserved <- pollWatch leafReady
inspectFull (case leafObserved of { WatchReady result -> responseValue result == "custody-leaf-reply"; _ -> False })
