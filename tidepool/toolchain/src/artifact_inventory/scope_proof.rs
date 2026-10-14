use super::*;

type BindingRows =
    BTreeMap<ArtifactBindingNode, BTreeSet<(ArtifactBindingNode, ArtifactDependency)>>;

/// An immutable cohort's complete outgoing rows, including sources with no
/// edges. Only sources issued in this scope occur in `nodes`; external targets
/// name exact earlier scopes. These facts grant no payload or compiler authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeProofV3 {
    pub selection: ArtifactSelectionId,
    pub nodes: Vec<ArtifactBindingNode>,
    pub dependencies: Vec<(ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency)>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum ScopeReference {
    Local(LogicalNodeKey),
    External(ArtifactBindingNode),
}

fn identity(rows: &BindingRows, own: ArtifactSelectionId) -> ArtifactSelectionId {
    let normalize = |node: ArtifactBindingNode| {
        if node.selection() == own {
            ScopeReference::Local(node.logical())
        } else {
            ScopeReference::External(node)
        }
    };
    let mut hash = Sha256::new();
    hash.update(b"Tidepool immutable binding scope proof v3\0");
    for (source, edges) in rows.iter().filter(|(source, _)| source.selection() == own) {
        let edges = edges
            .iter()
            .map(|(target, label)| (normalize(*target), label))
            .collect::<BTreeSet<_>>();
        let bytes = serde_json::to_vec(&(source.logical(), edges)).expect("closed scope schema");
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    ArtifactSelectionId(hash.finalize().into())
}

impl ScopeProofV3 {
    pub fn scope_dependencies(&self) -> BTreeSet<ArtifactSelectionId> {
        self.dependencies
            .iter()
            .map(|(_, target, _)| target.selection())
            .filter(|scope| *scope != self.selection)
            .collect()
    }

    pub(super) fn issue(rows: &BindingRows, provisional: ArtifactSelectionId) -> Option<Self> {
        if !rows.keys().any(|source| source.selection() == provisional) {
            return None;
        }
        let selection = identity(rows, provisional);
        let bind = |node: ArtifactBindingNode| {
            if node.selection() == provisional {
                node.logical().bind(selection)
            } else {
                node
            }
        };
        let own = rows
            .iter()
            .filter(|(source, _)| source.selection() == provisional)
            .map(|(source, edges)| {
                (
                    bind(*source),
                    edges
                        .iter()
                        .map(|(target, label)| (bind(*target), label.clone()))
                        .collect::<BTreeSet<_>>(),
                )
            })
            .collect::<BindingRows>();
        Some(Self {
            selection,
            nodes: own.keys().copied().collect(),
            dependencies: own
                .iter()
                .flat_map(|(source, edges)| {
                    edges
                        .iter()
                        .map(|(target, label)| (*source, *target, label.clone()))
                })
                .collect(),
        })
    }

    fn authenticated_rows(&self) -> Result<BindingRows, CompileError> {
        if self.nodes.is_empty()
            || self.nodes.windows(2).any(|pair| pair[0] >= pair[1])
            || self
                .nodes
                .iter()
                .any(|node| node.selection() != self.selection)
            || self.dependencies.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(failure("noncanonical binding scope proof sources"));
        }
        let mut rows = self
            .nodes
            .iter()
            .copied()
            .map(|source| (source, BTreeSet::new()))
            .collect::<BindingRows>();
        for (source, target, label) in &self.dependencies {
            let row = rows
                .get_mut(source)
                .ok_or_else(|| failure("binding scope proof source outside own cohort"))?;
            row.insert((*target, label.clone()));
        }
        if identity(&rows, self.selection) != self.selection {
            return Err(failure(
                "binding scope proof digest differs from own complete rows",
            ));
        }
        Ok(rows)
    }
}

pub(super) fn authenticate(
    proofs: &[Arc<ScopeProofV3>],
    selected: &BindingRows,
) -> Result<BTreeMap<ArtifactSelectionId, (Arc<ScopeProofV3>, BindingRows)>, CompileError> {
    let mut authenticated = BTreeMap::new();
    for proof in proofs {
        let rows = proof.authenticated_rows()?;
        if authenticated
            .insert(proof.selection, (Arc::clone(proof), rows))
            .is_some()
        {
            return Err(failure("duplicate binding scope proof"));
        }
    }
    for (proof, _) in authenticated.values() {
        for (_, target, _) in &proof.dependencies {
            if !authenticated
                .get(&target.selection())
                .is_some_and(|(_, rows)| rows.contains_key(target))
            {
                return Err(failure(
                    "binding scope proof target missing from exact ancestor",
                ));
            }
        }
    }
    // Remove dependency-free scopes iteratively. Artifact cycles within one
    // cohort do not participate in this immutable scope ancestry check.
    let mut unresolved = BTreeMap::new();
    let mut dependents = BTreeMap::<ArtifactSelectionId, Vec<ArtifactSelectionId>>::new();
    for (scope, (proof, _)) in &authenticated {
        let ancestors = proof.scope_dependencies();
        unresolved.insert(*scope, ancestors.len());
        for ancestor in ancestors {
            dependents.entry(ancestor).or_default().push(*scope);
        }
    }
    let mut pending = unresolved
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(scope, _)| *scope)
        .collect::<Vec<_>>();
    let mut completed = 0;
    while let Some(scope) = pending.pop() {
        completed += 1;
        for child in dependents.get(&scope).into_iter().flatten() {
            let count = unresolved
                .get_mut(child)
                .expect("authenticated child scope");
            *count -= 1;
            if *count == 0 {
                pending.push(*child);
            }
        }
    }
    if completed != authenticated.len() {
        return Err(failure("binding scope ancestry cycle"));
    }
    let mut required = BTreeSet::new();
    let mut pending = selected
        .keys()
        .map(|source| source.selection())
        .collect::<Vec<_>>();
    while let Some(scope) = pending.pop() {
        if required.insert(scope) {
            let (proof, _) = authenticated
                .get(&scope)
                .ok_or_else(|| failure("selected binding scope proof missing"))?;
            pending.extend(proof.scope_dependencies());
        }
    }
    if required.len() != authenticated.len() {
        return Err(failure("unused binding scope proof"));
    }
    for (source, row) in selected {
        if authenticated
            .get(&source.selection())
            .and_then(|(_, rows)| rows.get(source))
            != Some(row)
        {
            return Err(failure(
                "selected outgoing rows differ from issued scope proof",
            ));
        }
    }
    Ok(authenticated)
}

pub(super) fn prepare_install(
    state: &InventoryState,
    proofs: impl IntoIterator<Item = Arc<ScopeProofV3>>,
) -> Result<BTreeMap<ArtifactSelectionId, Arc<ScopeProofV3>>, CompileError> {
    let additions = proofs
        .into_iter()
        .map(|proof| (proof.selection, proof))
        .collect::<BTreeMap<_, _>>();
    for (scope, proof) in &additions {
        if state
            .selection_plans
            .get(scope)
            .is_some_and(|existing| existing.proof() != proof)
        {
            return Err(failure("scope identity has differing immutable proofs"));
        }
        for ancestor in proof.scope_dependencies() {
            if !state.selection_plans.contains_key(&ancestor) && !additions.contains_key(&ancestor)
            {
                return Err(failure("issuing binding scope ancestor proof missing"));
            }
        }
    }
    Ok(additions)
}

pub(super) fn install(
    state: &mut InventoryState,
    additions: BTreeMap<ArtifactSelectionId, Arc<ScopeProofV3>>,
) {
    for (scope, proof) in additions {
        if !state.selection_plans.contains_key(&scope) {
            state
                .issued_scope_nodes
                .fetch_add(proof.nodes.len() as u64, Ordering::Relaxed);
            state
                .selection_plans
                .insert(scope, ImmutableScopeProofRef::from_issued(proof));
        }
    }
}

#[cfg(test)]
mod tests;
