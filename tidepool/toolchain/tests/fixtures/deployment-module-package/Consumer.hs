{-# LANGUAGE NoImplicitPrelude #-}
module DeploymentPackageConsumer where
import Tidepool.Prelude

result :: Int
result = length (sort [41, 2, 3]) + 39
