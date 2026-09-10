use std::collections::{HashMap, VecDeque};

pub(super) type Key = (String, String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pool {
    General,
    Protected,
}

/// One FIFO for general work, with an independent protected path for small jobs.
/// Protected admission never consumes memory reserved for the general FIFO head.
pub(super) struct Queue {
    waiting: VecDeque<(Key, u64)>,
    active: HashMap<Key, (Pool, u64)>,
    general: u64,
    protected: u64,
    small: u64,
    general_used: u64,
    protected_used: u64,
}

impl Queue {
    pub(super) fn new(general: u64, protected: u64, small: u64) -> Self {
        Self {
            waiting: VecDeque::new(),
            active: HashMap::new(),
            general,
            protected,
            small,
            general_used: 0,
            protected_used: 0,
        }
    }

    pub(super) fn push(&mut self, key: Key, bytes: u64) {
        self.waiting.push_back((key, bytes));
    }

    pub(super) fn next(&mut self) -> Option<(Key, u64)> {
        let protected = self.waiting.iter().position(|(_, bytes)| {
            *bytes <= self.small && *bytes <= self.protected - self.protected_used
        });
        let (position, pool) = if let Some(position) = protected {
            (position, Pool::Protected)
        } else if self.waiting.front()?.1 <= self.general - self.general_used {
            (0, Pool::General)
        } else {
            return None;
        };
        let (key, bytes) = self.waiting.remove(position)?;
        match pool {
            Pool::General => self.general_used += bytes,
            Pool::Protected => self.protected_used += bytes,
        }
        self.active.insert(key.clone(), (pool, bytes));
        Some((key, bytes))
    }

    pub(super) fn release(&mut self, key: &Key) {
        self.waiting.retain(|(waiting, _)| waiting != key);
        if let Some((pool, bytes)) = self.active.remove(key) {
            match pool {
                Pool::General => self.general_used -= bytes,
                Pool::Protected => self.protected_used -= bytes,
            }
        }
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
