\target stale denied -> do
      rejected <- send (NotifyWith stale "must not reach host")
      case rejected of
        Left NotificationUnauthorized | denied -> pure (37 :: Int)
        Left NotificationUnavailable | not denied -> do
          after <- send (NotifyWith target "continued after stale rejection")
          case after of
            Left NotificationUnavailable -> pure 37
            _ -> error "unexpected test host result"
        _ -> error "wrong notification rejection"
