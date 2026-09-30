//! Validated immutable definitions share storage and compute their lookup hash
//! once. Hash equality never replaces exact structural equality.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(super) struct SharedContent<T> {
    value: Arc<T>,
    hash: u64,
}

impl<T: Hash> SharedContent<T> {
    pub(super) fn new(value: T) -> Self {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        Self {
            value: Arc::new(value),
            hash: hasher.finish(),
        }
    }
}

impl<T> Deref for SharedContent<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> AsRef<T> for SharedContent<T> {
    fn as_ref(&self) -> &T {
        &self.value
    }
}

impl<T: Eq> PartialEq for SharedContent<T> {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
            && (Arc::ptr_eq(&self.value, &other.value) || self.value == other.value)
    }
}

impl<T: Eq> Eq for SharedContent<T> {}

impl<T> Hash for SharedContent<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clones_share_storage_and_colliding_hashes_still_compare_contents() {
        let value = SharedContent::new(vec![1, 2, 3]);
        let cloned = value.clone();
        assert!(Arc::ptr_eq(&value.value, &cloned.value));
        assert_eq!(value, cloned);
        let mut other = SharedContent::new(vec![1, 2, 4]);
        other.hash = value.hash;
        assert_ne!(value, other);
        assert_eq!(value, SharedContent::new(vec![1, 2, 3]));
    }
}
