use serde::{Deserialize, Serialize};
use tidepool_repr::PrincipalId;

/// Stable routing identity for an actor lineage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActorId(pub u64);

/// One concrete lifetime of an [`ActorId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Incarnation(pub u64);

impl Incarnation {
    pub const FIRST: Self = Self(1);
}

/// An exact actor incarnation. Operations never substitute another
/// incarnation merely because its protocol type is compatible.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ActorRef {
    pub id: ActorId,
    pub incarnation: Incarnation,
}

impl ActorRef {
    pub const fn first(id: ActorId) -> Self {
        Self {
            id,
            incarnation: Incarnation::FIRST,
        }
    }
}

/// `3@1`, the way every actor-facing surface spells an incarnation.
///
/// An agent reasons about actors by path and by this short form, and can do
/// nothing with a struct dump of the Rust representation.
impl std::fmt::Display for ActorRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}@{}", self.id.0, self.incarnation.0)
    }
}

/// `Debug` is `Display`: an `ActorRef` nested in an error, an `Option`, or a
/// status row formatted with `{:?}` reaches agents too, and must read `3@1`
/// there as well.
impl std::fmt::Debug for ActorRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

impl From<ActorRef> for PrincipalId {
    fn from(actor: ActorRef) -> Self {
        Self::new(actor.id.0, actor.incarnation.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{ActorId, ActorRef, Incarnation};

    #[test]
    fn actor_ref_debug_and_display_read_id_at_incarnation() {
        let actor = ActorRef {
            id: ActorId(2),
            incarnation: Incarnation(1),
        };
        assert_eq!(format!("{actor}"), "2@1");
        assert_eq!(format!("{actor:?}"), "2@1");
        assert_eq!(format!("{:?}", Some(actor)), "Some(2@1)");
    }
}
