Right uncertain <- traverse R.start (workDefinition [("producer", R.settlement producer, R.progress updates)] (WorkSink $ \_ event -> case workMessage id event of
      Nothing -> pure noWorkDelivery
      Just _ -> pure (WorkDelivery (Just (Left (NotificationAdmissionUnconfirmed "controlled lost admission acknowledgment"))) [])))
