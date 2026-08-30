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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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

impl From<ActorRef> for PrincipalId {
    fn from(actor: ActorRef) -> Self {
        Self::new(actor.id.0, actor.incarnation.0)
    }
}
