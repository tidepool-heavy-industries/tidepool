let sendOne :: Exomonad.AgentRef -> Eff '[Exomonad.Notifications] (Either Exomonad.NotificationError Exomonad.NotificationReceipt)
    sendOne target = Exomonad.sendMessage target "notice"
    observeOne :: Exomonad.NotificationReceipt -> Eff '[Exomonad.Notifications] (Either Exomonad.NotificationError Exomonad.NotificationState)
    observeOne = Exomonad.pollNotification
in pure (sendOne, observeOne)
