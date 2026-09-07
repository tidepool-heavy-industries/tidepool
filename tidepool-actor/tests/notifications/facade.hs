let sendOne :: Shoal.AgentRef -> Eff '[Shoal.Notifications] (Either Shoal.NotificationError Shoal.NotificationReceipt)
    sendOne target = Shoal.notify target "notice"
    observeOne :: Shoal.NotificationReceipt -> Eff '[Shoal.Notifications] (Either Shoal.NotificationError Shoal.NotificationState)
    observeOne = Shoal.pollNotification
in pure (sendOne, observeOne)
