//! Frozen post-coalgebra context snapshots — the shared prefix a set of
//! branches fork from, named and digested.
//!
//! # The boundary already existed
//!
//! [`crate::harness::Harness::register_fork_child`] has always computed
//! `checkpoint = parent_transcript.len()` and seeded the child with the
//! parent's transcript AND framing, and [`crate::engine::assemble_request`]
//! is `[system(framing ?? SYSTEM_FRAMING)] ++ transcript` — verbatim,
//! unreordered, untrimmed. So a fork child's assembled request prefix is
//! ALREADY byte-identical to its parent's through the checkpoint. This module
//! does not invent that prefix; it gives it an identity ([`SnapshotDigest`]),
//! a shared representation (`Arc<[Message]>`), and receipts.
//!
//! # Why the digest runs over the ASSEMBLED prefix
//!
//! [`digest_prefix`] calls [`crate::engine::assemble_request`] itself rather
//! than re-deriving "framing first, then the transcript". There is one
//! assembly path, so the digest cannot drift from what a provider is actually
//! sent: a change to assembly is a change to the digest, loudly, rather than
//! a silent divergence between what we hash and what we send. That is also
//! what makes the byte-stability assertion meaningful — a test re-digests a
//! CHILD's own assembled prefix through the same function and compares.
//!
//! # Immutability
//!
//! A [`ContextSnapshot`] is never mutated (PRD 21 locked decision 2). Once it
//! has children its prefix is never rewritten; compaction of a node that has
//! one mints a NEW snapshot with a NEW digest and leaves the existing interned
//! entry — and therefore every existing child — untouched. `Arc<[Message]>` is
//! the representation that makes this cheap and makes it true by construction:
//! there is no `&mut` path to a frozen prefix.
//!
//! # What the digest does NOT claim
//!
//! It is OUR identity for a context prefix, not a provider cache key. Nothing
//! in this tree emits `cache_control` breakpoints and no provider impl reports
//! a cache-read metric we can attribute to a digest, so a matching digest
//! proves the bytes are the same, never that a provider reused anything. See
//! `tidepool-harness/CLAUDE.md`, "The provider cache-metric gap".

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::provider::{Message, Role};

/// Domain separator for the snapshot digest — mirrors
/// `tidepool_runtime::cache`'s `INVOCATION_NAMESPACE` idiom so a snapshot
/// digest can never collide with a compile-cache key computed over
/// coincidentally-equal bytes. Versioned: a change to what the digest covers
/// bumps this rather than silently re-meaning old digests.
const SNAPSHOT_NAMESPACE: &[u8] = b"tidepool-context-snapshot-v1";

/// The blake3 identity of a frozen context prefix, hex-encoded. A newtype for
/// the same reason `tidepool_runtime::cache::InvocationKey` is one: a raw
/// string must not be mistaken for a computed digest at an intern/lookup
/// boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SnapshotDigest(pub String);

impl SnapshotDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SnapshotDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A frozen post-coalgebra context prefix: the exact `[framing] ++ messages`
/// [`crate::engine::assemble_request`] re-emits, plus its identity.
///
/// `messages` is the node's TRANSCRIPT at freeze time (the framing is carried
/// separately, exactly as a `NodeConvo` carries it) — the assembled prefix is
/// `assemble_request(&messages, _, framing)`, one message longer.
///
/// `Arc<[Message]>` rather than `Vec<Message>`: the durable model is already
/// reference-shaped (`Event::TurnForked` records a POSITION, not a copy, and
/// `replay.rs` never materializes an inherited prefix) while the live model
/// clones per child. Sharing the frozen prefix aligns the two and makes the
/// digest a once-per-snapshot cost rather than a per-child one.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextSnapshot {
    /// Identity over the assembled prefix — see [`digest_prefix`].
    pub digest: SnapshotDigest,
    /// The node's own system message, or `None` for the default
    /// [`crate::engine::SYSTEM_FRAMING`]. Covered by the digest either way:
    /// `None` and an explicit framing equal to the default hash IDENTICALLY,
    /// because the digest runs over the ASSEMBLED messages, where both have
    /// already resolved to the same system content.
    pub framing: Option<String>,
    /// The frozen transcript prefix. Never mutated.
    pub messages: Arc<[Message]>,
    /// The node's `turn_seq` at freeze time — which turn boundary this cache
    /// root sits at, for a receipt reader correlating it with `TurnDelta`s.
    pub frozen_at_turn: u64,
}

impl ContextSnapshot {
    /// Freeze `transcript` under `framing`, computing the digest.
    pub fn freeze(framing: Option<String>, transcript: Vec<Message>, frozen_at_turn: u64) -> Self {
        let messages: Arc<[Message]> = transcript.into();
        let digest = digest_prefix(framing.as_deref(), &messages);
        ContextSnapshot {
            digest,
            framing,
            messages,
            frozen_at_turn,
        }
    }

    /// Total UTF-8 CONTENT bytes of the assembled prefix — the system framing
    /// message included, since that is genuinely re-sent every turn.
    ///
    /// Bytes, not tokens, and deliberately: there is no local tokenizer in
    /// this tree, so a token-level split of the prefix would be an estimate
    /// wearing a receipt's clothes. Byte counts are exact and any reader can
    /// recompute them. Role tags are not counted — they are wire framing, not
    /// content.
    pub fn prefix_bytes(&self) -> u64 {
        content_bytes(&assembled(self.framing.as_deref(), &self.messages))
    }

    /// The assembled prefix itself — `[system] ++ messages`, exactly what a
    /// child re-emits through its own [`crate::engine::assemble_request`] call
    /// for its first `messages.len() + 1` messages.
    pub fn assembled_prefix(&self) -> Vec<Message> {
        assembled(self.framing.as_deref(), &self.messages)
    }
}

/// The digest of the prefix `[system(framing)] ++ transcript`, as
/// [`crate::engine::assemble_request`] assembles it.
pub fn digest_prefix(framing: Option<&str>, transcript: &[Message]) -> SnapshotDigest {
    digest_messages(&assembled(framing, transcript))
}

/// The digest of an ALREADY-ASSEMBLED message list — the primitive
/// [`digest_prefix`] is defined in terms of, and the one a byte-stability
/// check calls directly on a child's own assembled request prefix.
///
/// Covers, length-framed: the domain separator, the message count, then each
/// message's role tag and content in order. Length framing (rather than NUL
/// separation) for the reason `tidepool_runtime::cache::frame` records: a NUL
/// embedded in one field would otherwise shift bytes across a boundary and
/// make two different prefixes hash alike. `reasoning_items` is deliberately
/// NOT covered: it is `#[serde(skip)]`, in-memory only, and never part of what
/// a fork child inherits.
pub fn digest_messages(messages: &[Message]) -> SnapshotDigest {
    let mut hasher = blake3::Hasher::new();
    frame(&mut hasher, SNAPSHOT_NAMESPACE);
    frame(&mut hasher, &(messages.len() as u64).to_le_bytes());
    for m in messages {
        frame(&mut hasher, role_tag(&m.role).as_bytes());
        frame(&mut hasher, m.content.as_bytes());
    }
    SnapshotDigest(hasher.finalize().to_hex().to_string())
}

/// Total UTF-8 content bytes of a message list — the unit every snapshot
/// receipt's byte counts are in (see [`ContextSnapshot::prefix_bytes`]).
pub fn content_bytes(messages: &[Message]) -> u64 {
    messages.iter().map(|m| m.content.len() as u64).sum()
}

/// `[system(framing)] ++ transcript`, via the ONE assembly path.
fn assembled(framing: Option<&str>, transcript: &[Message]) -> Vec<Message> {
    crate::engine::assemble_request(transcript, None, framing).messages
}

/// Hash a length-prefixed field: unambiguous framing regardless of content.
/// The same three lines as `tidepool_runtime::cache::frame`, which is private
/// — reimplemented rather than a second, differently-shaped scheme invented.
fn frame(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// The stable wire tag a role contributes to the digest. Spelled out here
/// rather than taken from serde so a rename of the log's wire spelling cannot
/// silently re-key every snapshot.
fn role_tag(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.to_string(),
            reasoning_items: Vec::new(),
        }
    }

    #[test]
    fn digest_is_stable_for_an_unchanged_prefix() {
        let t = vec![msg(Role::User, "a"), msg(Role::Assistant, "b")];
        assert_eq!(digest_prefix(Some("f"), &t), digest_prefix(Some("f"), &t));
    }

    /// The framing is genuinely covered — two identical transcripts under
    /// different system messages are different cache roots.
    #[test]
    fn framing_participates_in_the_digest() {
        let t = vec![msg(Role::User, "a")];
        assert_ne!(digest_prefix(Some("f1"), &t), digest_prefix(Some("f2"), &t));
        assert_ne!(digest_prefix(None, &t), digest_prefix(Some("f1"), &t));
    }

    /// `None` framing resolves to `SYSTEM_FRAMING` in assembly, so it must
    /// digest identically to passing that string explicitly — the digest
    /// describes what is SENT, not how the caller spelled it.
    #[test]
    fn none_framing_digests_as_the_default_framing() {
        let t = vec![msg(Role::User, "a")];
        assert_eq!(
            digest_prefix(None, &t),
            digest_prefix(Some(crate::engine::SYSTEM_FRAMING), &t)
        );
    }

    /// What length framing buys: content shifted across a message boundary
    /// must not hash alike.
    #[test]
    fn length_framing_separates_shifted_content() {
        let a = vec![msg(Role::User, "ab"), msg(Role::User, "c")];
        let b = vec![msg(Role::User, "a"), msg(Role::User, "bc")];
        assert_ne!(digest_messages(&a), digest_messages(&b));
    }

    /// Role is covered: the same text from the user and from the assistant
    /// are different contexts.
    #[test]
    fn role_participates_in_the_digest() {
        let a = vec![msg(Role::User, "x")];
        let b = vec![msg(Role::Assistant, "x")];
        assert_ne!(digest_messages(&a), digest_messages(&b));
    }

    /// A snapshot's own `assembled_prefix` re-digests to its own digest —
    /// the in-module half of the byte-stability contract the integration
    /// suite asserts across a real parent/child pair.
    #[test]
    fn assembled_prefix_redigests_to_the_snapshot_digest() {
        let snap = ContextSnapshot::freeze(
            Some("framing".to_string()),
            vec![msg(Role::User, "a"), msg(Role::Assistant, "b")],
            2,
        );
        assert_eq!(digest_messages(&snap.assembled_prefix()), snap.digest);
    }

    /// `reasoning_items` is in-memory only and never inherited — it must not
    /// move the digest.
    #[test]
    fn reasoning_items_do_not_move_the_digest() {
        let plain = vec![msg(Role::Assistant, "b")];
        let mut with_items = plain.clone();
        with_items[0].reasoning_items = vec![crate::provider::ReasoningItem(serde_json::json!({
            "type": "reasoning",
            "encrypted_content": "opaque",
        }))];
        assert_eq!(digest_messages(&plain), digest_messages(&with_items));
    }

    #[test]
    fn prefix_bytes_counts_framing_and_every_message() {
        let snap = ContextSnapshot::freeze(Some("fr".to_string()), vec![msg(Role::User, "abc")], 1);
        assert_eq!(snap.prefix_bytes(), 2 + 3);
    }
}
