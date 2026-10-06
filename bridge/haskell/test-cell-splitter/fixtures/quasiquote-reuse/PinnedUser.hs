module PinnedUser where

import qualified Data.Text as Text
import PinnedQuoted (value)

result :: Int
result = if value == Text.pack "https://example.test/stable" then 23 else 0
