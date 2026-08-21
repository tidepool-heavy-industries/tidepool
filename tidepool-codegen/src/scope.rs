//! The scope tree — one shared spine for both session planes (PRD 21 lane
//! C2, `plans/self-iterating-harness/21-c2-scope-trees.md`).
//!
//! Locked decision 4 asks for a *persistent lexical environment*: everything
//! immutable, "write" meaning "create a descendant scope", lookup walking
//! local → parent, siblings shadowing freely and never colliding, and the
//! parent never gaining a child's names. This module is the whole of that
//! structure; the value plane ([`crate::binding_table::BindingTable`]) and
//! the decl plane (`tidepool_runtime::session::SessionLib`) each hang their
//! own frames off these ids rather than growing separate nesting.
//!
//! ## Identity
//!
//! [`ScopeId`] is a monotone counter, **never reused**. That is the whole of
//! "stable identity" (PRD 21 open question 1): a retired scope's id is never
//! re-minted, so a stale reference is detectably dead rather than silently
//! aliased onto a live scope. A scope's parent is fixed at mint time and
//! never rewritten.
//!
//! ## Flat sessions are the root scope
//!
//! [`ScopeId::ROOT`] is the flat session every pre-C2 caller already lives
//! in. Each scope-taking API in either plane has a no-arg sibling meaning
//! ROOT, and that sibling keeps its exact prior behavior — the back-compat
//! contract, discharged by the proof obligations in the design doc's §5.

use std::collections::HashMap;

/// A node in the scope tree — an invocation-local declaration/binding frame.
///
/// Monotone and never reused (see the module docs). [`ScopeId::ROOT`] is the
/// flat session.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeId(pub u64);

impl ScopeId {
    /// The flat session — every pre-C2 caller's scope, and the root of every
    /// lookup walk.
    pub const ROOT: ScopeId = ScopeId(0);

    /// Whether this is the root scope.
    #[must_use]
    pub fn is_root(self) -> bool {
        self == ScopeId::ROOT
    }
}

/// The parent map. `ROOT` is present from construction and has no parent;
/// every other scope is minted as some existing scope's child.
///
/// Retirement removes a scope from the tree but never reclaims its id, so a
/// [`ScopeTree::parent_of`] on a retired scope answers `None` — the same
/// answer as for an id that was never minted, which is the correct one: in
/// both cases there is no live frame to resolve through.
#[derive(Debug, Clone)]
pub struct ScopeTree {
    /// child → parent. `ROOT` is never a key.
    parent: HashMap<ScopeId, ScopeId>,
    /// Next id to mint. Monotone; never decremented by retirement.
    next: u64,
}

impl Default for ScopeTree {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeTree {
    /// A tree containing only [`ScopeId::ROOT`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            parent: HashMap::new(),
            next: 1,
        }
    }

    /// Mint a fresh child of `parent`. Returns `None` if `parent` is not live
    /// (never minted, or already retired) — a scope can never be born under a
    /// dead ancestor, which is what keeps every live scope's walk terminating
    /// at ROOT.
    pub fn mint_child(&mut self, parent: ScopeId) -> Option<ScopeId> {
        if !self.is_live(parent) {
            return None;
        }
        let id = ScopeId(self.next);
        self.next += 1;
        self.parent.insert(id, parent);
        Some(id)
    }

    /// Whether `scope` is a live node of this tree.
    #[must_use]
    pub fn is_live(&self, scope: ScopeId) -> bool {
        scope.is_root() || self.parent.contains_key(&scope)
    }

    /// `scope`'s parent, or `None` for ROOT and for any non-live scope.
    #[must_use]
    pub fn parent_of(&self, scope: ScopeId) -> Option<ScopeId> {
        self.parent.get(&scope).copied()
    }

    /// `scope` then each ancestor up to and including ROOT — the resolution
    /// order both planes walk (local first, parent last). Empty for a
    /// non-live scope.
    #[must_use]
    pub fn lookup_chain(&self, scope: ScopeId) -> Vec<ScopeId> {
        if !self.is_live(scope) {
            return Vec::new();
        }
        let mut chain = vec![scope];
        let mut cur = scope;
        while let Some(p) = self.parent_of(cur) {
            chain.push(p);
            cur = p;
        }
        chain
    }

    /// `scope`'s live descendants, deepest first — the order retirement must
    /// visit so a child's frames are gone before its parent's.
    ///
    /// Excludes `scope` itself.
    #[must_use]
    pub fn descendants_deepest_first(&self, scope: ScopeId) -> Vec<ScopeId> {
        let mut depth: Vec<(usize, ScopeId)> = Vec::new();
        for (&child, _) in self.parent.iter() {
            let chain = self.lookup_chain(child);
            if let Some(pos) = chain.iter().position(|&s| s == scope) {
                // `pos` is the number of hops from `child` up to `scope`;
                // 0 means `child == scope`, which we exclude.
                if pos > 0 {
                    depth.push((pos, child));
                }
            }
        }
        // Deepest (largest hop count) first; ties broken by id for a
        // deterministic order regardless of HashMap iteration.
        depth.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        depth.into_iter().map(|(_, s)| s).collect()
    }

    /// Retire `scope`, returning it and every live descendant, deepest
    /// first — the exact set whose frames and roots the caller must now
    /// release, in the order it must release them.
    ///
    /// ROOT is never retired (the flat session outlives every scope);
    /// retiring it is a no-op returning an empty list, as is retiring a
    /// non-live scope. Ids are never returned to the pool.
    pub fn retire(&mut self, scope: ScopeId) -> Vec<ScopeId> {
        if scope.is_root() || !self.is_live(scope) {
            return Vec::new();
        }
        let mut doomed = self.descendants_deepest_first(scope);
        doomed.push(scope);
        for s in &doomed {
            self.parent.remove(s);
        }
        doomed
    }

    /// Number of live scopes, ROOT included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.parent.len() + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_live_and_parentless() {
        let t = ScopeTree::new();
        assert!(t.is_live(ScopeId::ROOT));
        assert_eq!(t.parent_of(ScopeId::ROOT), None);
        assert_eq!(t.lookup_chain(ScopeId::ROOT), vec![ScopeId::ROOT]);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn lookup_chain_walks_local_to_root() {
        let mut t = ScopeTree::new();
        let a = t.mint_child(ScopeId::ROOT).expect("root is live");
        let b = t.mint_child(a).expect("a is live");
        assert_eq!(t.lookup_chain(b), vec![b, a, ScopeId::ROOT]);
    }

    /// Siblings never see each other: neither appears in the other's chain.
    /// This is the representational half of locked decision 4's "siblings
    /// shadow freely and never collide".
    #[test]
    fn siblings_are_mutually_invisible() {
        let mut t = ScopeTree::new();
        let l = t.mint_child(ScopeId::ROOT).expect("root is live");
        let r = t.mint_child(ScopeId::ROOT).expect("root is live");
        assert!(!t.lookup_chain(l).contains(&r));
        assert!(!t.lookup_chain(r).contains(&l));
        assert_eq!(t.lookup_chain(l), vec![l, ScopeId::ROOT]);
    }

    /// Nothing ever walks downward — the parent's chain is unchanged by a
    /// child existing, which is why "the parent never gains child
    /// declarations by name" needs no enforcement check.
    #[test]
    fn a_child_does_not_appear_in_its_parents_chain() {
        let mut t = ScopeTree::new();
        let c = t.mint_child(ScopeId::ROOT).expect("root is live");
        assert_eq!(t.lookup_chain(ScopeId::ROOT), vec![ScopeId::ROOT]);
        assert!(!t.lookup_chain(ScopeId::ROOT).contains(&c));
    }

    #[test]
    fn retire_returns_the_subtree_deepest_first_and_unlives_it() {
        let mut t = ScopeTree::new();
        let a = t.mint_child(ScopeId::ROOT).expect("root");
        let b = t.mint_child(a).expect("a");
        let c = t.mint_child(b).expect("b");
        let sib = t.mint_child(ScopeId::ROOT).expect("root");

        let doomed = t.retire(a);
        assert_eq!(
            doomed,
            vec![c, b, a],
            "deepest first, then the scope itself"
        );
        for s in [a, b, c] {
            assert!(!t.is_live(s));
            assert_eq!(t.lookup_chain(s), Vec::new());
        }
        assert!(t.is_live(sib), "a sibling subtree is untouched");
        assert!(t.is_live(ScopeId::ROOT));
    }

    /// Ids are never reused, so a stale reference to a retired scope stays
    /// detectably dead instead of aliasing onto a later scope.
    #[test]
    fn retired_ids_are_never_reminted() {
        let mut t = ScopeTree::new();
        let a = t.mint_child(ScopeId::ROOT).expect("root");
        t.retire(a);
        let b = t.mint_child(ScopeId::ROOT).expect("root");
        assert_ne!(a, b);
        assert!(!t.is_live(a));
    }

    #[test]
    fn a_dead_scope_cannot_parent_a_new_one() {
        let mut t = ScopeTree::new();
        let a = t.mint_child(ScopeId::ROOT).expect("root");
        t.retire(a);
        assert_eq!(t.mint_child(a), None);
        assert_eq!(t.mint_child(ScopeId(999)), None, "never-minted id too");
    }

    #[test]
    fn root_never_retires() {
        let mut t = ScopeTree::new();
        let a = t.mint_child(ScopeId::ROOT).expect("root");
        assert_eq!(t.retire(ScopeId::ROOT), Vec::new());
        assert!(t.is_live(ScopeId::ROOT));
        assert!(t.is_live(a), "and nothing under it is disturbed");
    }

    #[test]
    fn retiring_a_dead_scope_is_a_no_op() {
        let mut t = ScopeTree::new();
        let a = t.mint_child(ScopeId::ROOT).expect("root");
        assert_eq!(t.retire(a), vec![a]);
        assert_eq!(t.retire(a), Vec::new(), "idempotent");
    }
}
