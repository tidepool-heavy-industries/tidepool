uncertain <- followWork [("producer", forkedResponse producer, updates)] (\event -> case workMessage id event of
      Nothing -> pure Nothing
      Just _ -> pure (Just (Left (NotificationAdmissionUnconfirmed "controlled lost admission acknowledgment"))))
