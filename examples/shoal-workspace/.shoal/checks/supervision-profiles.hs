import qualified Project.SupervisionProfiles as Profiles
import qualified Project.Watchdog as Watchdog

let reviewerPolicy =
      Profiles.extendRole
        Profiles.Reviewer
        [Watchdog.stayWithin "tidepool-actor"]

let explicitlyAssigned path
      | path == "review-wave/exact-reviewer" = reviewerPolicy
      | otherwise = []

( Profiles.roleName Profiles.Reviewer
  , map Watchdog.heuristicName (explicitlyAssigned "review-wave/exact-reviewer")
  , map Watchdog.heuristicName (explicitlyAssigned "review-wave/not-assigned")
  )
