Right uncertain <- followWork [("producer", producer, updates)] (WorkSink $ \_ event -> case workMessage id event of
      Nothing -> pure noWorkDelivery
      Just _ -> pure (WorkDelivery (Just (Left (NotificationAdmissionUnconfirmed "controlled lost admission acknowledgment"))) []))
