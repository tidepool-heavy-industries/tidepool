//! The low-bit ABI for managed references.
//!
//! Managed allocations are at least eight-byte aligned, so the low three bits
//! of a reference carry evaluatedness evidence.  This module is the only owner
//! of the bit operations; users must untag before treating a reference as an
//! address and must preserve the word when moving a managed edge.

use crate::execution_descriptor::{DescriptorState, ObjectKind};
use std::num::NonZeroU32;

/// Number of low bits reserved for evaluatedness evidence.
pub const TAG_BITS: usize = 3;
/// Mask for the managed-reference evidence bits.
pub const TAG_MASK: usize = (1 << TAG_BITS) - 1;
/// No evaluatedness evidence.
pub const UNEVALUATED_TAG: u8 = 0;
/// Generic evaluated/descriptor evidence for constructors, functions, and PAPs.
pub const DESCRIPTOR_TAG: u8 = 7;

/// Extract the managed-reference evidence from an encoded machine word.
#[inline]
pub const fn tag_of(value: usize) -> u8 {
    (value & TAG_MASK) as u8
}

/// Remove managed-reference evidence from an encoded machine word.
#[inline]
pub const fn untag(value: usize) -> usize {
    value & !TAG_MASK
}

/// Whether an evidence value is representable by this ABI.
#[inline]
pub const fn tag_bits_valid(tag: u8) -> bool {
    (tag as usize) <= TAG_MASK
}

/// Validate evidence against the final descriptor and header state.
///
/// Zero is deliberately inconclusive and is valid for every shape.  Small
/// nonzero tags identify only constructors with the exact authoritative tag;
/// tag seven identifies any live constructor by descriptor inspection, or a
/// function/PAP descriptor.  Any nonzero evidence on a thunk,
/// continuation, or non-live object is contradictory.
#[inline]
pub const fn tag_valid(
    tag: u8,
    kind: ObjectKind,
    state: DescriptorState,
    constructor_tag: Option<NonZeroU32>,
) -> bool {
    if !tag_bits_valid(tag) {
        return false;
    }
    if tag == UNEVALUATED_TAG {
        return true;
    }
    if !matches!(state, DescriptorState::Live) {
        return false;
    }
    match (tag, kind) {
        (1..=6, ObjectKind::Constructor) => {
            matches!(constructor_tag, Some(value) if value.get() == tag as u32)
        }
        (DESCRIPTOR_TAG, ObjectKind::Constructor) => constructor_tag.is_some(),
        (DESCRIPTOR_TAG, ObjectKind::Function | ObjectKind::Pap | ObjectKind::External(_)) => true,
        _ => false,
    }
}

/// Validate an encoded managed word without consulting its descriptor.
///
/// A null managed edge is represented only by the all-zero word.  In
/// particular, a tagged null is malformed.  Descriptor-specific evidence is
/// checked by the collector once the final descriptor is known.
#[inline]
pub const fn word_valid(value: usize) -> bool {
    value == 0 || (untag(value) != 0 && tag_bits_valid(tag_of(value)))
}

/// Canonical evidence for an authoritative one-based constructor tag.
#[inline]
pub const fn constructor_tag(tag: u32) -> Option<u8> {
    match tag {
        0 => None,
        1..=6 => Some(tag as u8),
        _ => Some(DESCRIPTOR_TAG),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_bits_round_trip_without_touching_address_bits() {
        let address = 0x1234_5678_9abc_def0usize;
        for tag in 0..=TAG_MASK as u8 {
            let encoded = address | usize::from(tag);
            assert_eq!(tag_of(encoded), tag);
            assert_eq!(untag(encoded), address);
            assert!(tag_bits_valid(tag));
        }
        assert_eq!(untag(0xffff_ffff_ffff_fff8usize | 7), 0xffff_ffff_ffff_fff8);
    }

    #[test]
    fn tagged_null_is_invalid() {
        assert!(word_valid(0));
        for tag in 1..=TAG_MASK {
            assert!(!word_valid(tag));
        }
    }

    #[test]
    fn constructor_tags_are_canonical() {
        assert_eq!(constructor_tag(0), None);
        assert_eq!(constructor_tag(1), Some(1));
        assert_eq!(constructor_tag(6), Some(6));
        assert_eq!(constructor_tag(7), Some(DESCRIPTOR_TAG));
        assert_eq!(constructor_tag(u32::MAX), Some(DESCRIPTOR_TAG));
    }

    #[test]
    fn descriptor_evidence_rejects_contradictory_tags() {
        let constructor = NonZeroU32::new(3);
        assert!(tag_valid(
            0,
            ObjectKind::Thunk,
            DescriptorState::Evaluating,
            None
        ));
        assert!(tag_valid(
            3,
            ObjectKind::Constructor,
            DescriptorState::Live,
            constructor
        ));
        assert!(!tag_valid(
            4,
            ObjectKind::Constructor,
            DescriptorState::Live,
            constructor
        ));
        assert!(!tag_valid(
            3,
            ObjectKind::Thunk,
            DescriptorState::Live,
            None
        ));
        assert!(tag_valid(
            7,
            ObjectKind::Function,
            DescriptorState::Live,
            None
        ));
        assert!(tag_valid(
            7,
            ObjectKind::Constructor,
            DescriptorState::Live,
            NonZeroU32::new(2)
        ));
        assert!(tag_valid(
            7,
            ObjectKind::Constructor,
            DescriptorState::Live,
            NonZeroU32::new(700)
        ));
        assert!(!tag_valid(
            7,
            ObjectKind::Function,
            DescriptorState::Evaluating,
            None
        ));
    }
}
