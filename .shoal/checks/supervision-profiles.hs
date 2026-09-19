import qualified Project.SupervisionProfiles as Profiles
import qualified Project.Watchdog as Watchdog

let explicitlyAssigned path =
      case path of
        "build-wave/implementer" ->
          Profiles.extendRole Profiles.Implementer [Watchdog.stayWithin "tidepool-actor"]
        "review-wave/reviewer" ->
          Profiles.roleHeuristics Profiles.Reviewer
        "research-wave/researcher" ->
          Profiles.roleHeuristics Profiles.Researcher
        _ -> []

( Profiles.roleName Profiles.Reviewer
  , map Watchdog.heuristicName (explicitlyAssigned "build-wave/implementer")
  , map Watchdog.heuristicName (explicitlyAssigned "review-wave/reviewer")
  , map Watchdog.heuristicName (explicitlyAssigned "research-wave/researcher")
  , map Watchdog.heuristicName (explicitlyAssigned "review-wave/reviewer-extra")
  )
