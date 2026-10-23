//! Original compiler completion belongs to its immutable output, not retained state.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
enum OriginalInstanceCensus {
    Complete,
    MissingOwners(Arc<[ExactModuleIdentity]>),
}

/// One actual output's execution custody and instance census. Native custody is
/// finalized before this owner is issued; context transformations cannot alter it.
#[derive(Debug)]
pub struct OriginalExecutionEvidence {
    context: Arc<ExactDeclarationContext>,
    target: ExactModuleIdentity,
    census: OriginalInstanceCensus,
    identity: OnceLock<[u8; 32]>,
}

/// Missing instance evidence does not invalidate executable native output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncompleteOriginalExecution {
    target: ExactModuleIdentity,
    missing_owners: Arc<[ExactModuleIdentity]>,
}

impl IncompleteOriginalExecution {
    pub fn target(&self) -> &ExactModuleIdentity {
        &self.target
    }
    pub fn missing_owners(&self) -> &[ExactModuleIdentity] {
        &self.missing_owners
    }
}
impl std::fmt::Display for IncompleteOriginalExecution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "original compiler execution for {}:{} lacks instance evidence for {:?}",
            self.target.unit, self.target.module, self.missing_owners
        )
    }
}
impl std::error::Error for IncompleteOriginalExecution {}
impl From<IncompleteOriginalExecution> for CompileError {
    fn from(error: IncompleteOriginalExecution) -> Self {
        CompileError::IncompleteOriginalExecution(Box::new(error))
    }
}

/// A complete view borrows the exact output owner, never a transferable flag.
#[derive(Clone, Copy, Debug)]
pub struct CompletedOriginalExecution<'a> {
    evidence: &'a Arc<OriginalExecutionEvidence>,
}

/// Original output and derived pure display have distinct issuing owners.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OriginalExecutionSource {
    Original(Arc<OriginalExecutionEvidence>),
    PurePreview(Arc<crate::activation_preview::OriginalDisplayContext>),
}
impl OriginalExecutionSource {
    pub fn context(&self) -> &Arc<ExactDeclarationContext> {
        match self {
            Self::Original(evidence) => evidence.context(),
            Self::PurePreview(display) => display.declarations(),
        }
    }
    pub fn identity(&self) -> [u8; 32] {
        match self {
            Self::Original(evidence) => evidence.identity(),
            Self::PurePreview(display) => display.identity(),
        }
    }
}

impl OriginalExecutionEvidence {
    pub fn context(&self) -> &Arc<ExactDeclarationContext> {
        &self.context
    }
    pub fn target(&self) -> &ExactModuleIdentity {
        &self.target
    }
    pub fn missing_owners(&self) -> &[ExactModuleIdentity] {
        match &self.census {
            OriginalInstanceCensus::Complete => &[],
            OriginalInstanceCensus::MissingOwners(owners) => owners,
        }
    }
    pub fn completed(
        self: &Arc<Self>,
    ) -> Result<CompletedOriginalExecution<'_>, IncompleteOriginalExecution> {
        match &self.census {
            OriginalInstanceCensus::Complete => Ok(CompletedOriginalExecution { evidence: self }),
            OriginalInstanceCensus::MissingOwners(owners) => Err(IncompleteOriginalExecution {
                target: self.target.clone(),
                missing_owners: Arc::clone(owners),
            }),
        }
    }
    pub fn identity(&self) -> [u8; 32] {
        *self.identity.get_or_init(|| {
            use sha2::{Digest, Sha256};
            let census = match &self.census {
                OriginalInstanceCensus::Complete => Value::Array(vec![text("complete")]),
                OriginalInstanceCensus::MissingOwners(owners) => Value::Array(vec![
                    text("missing"),
                    Value::Array(owners.iter().map(module_value).collect()),
                ]),
            };
            let facts = Value::Array(vec![
                text("TPEXACTEXECUTION1"),
                Value::Bytes(self.context.authenticated_observation_identity().to_vec()),
                module_value(&self.target),
                census,
            ]);
            let mut bytes = Vec::new();
            ciborium::ser::into_writer(&facts, &mut bytes).expect("owned execution observations");
            Sha256::digest(bytes).into()
        })
    }
    pub fn same_authenticated_execution(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
            || (self.target == other.target
                && self.census == other.census
                && self.context.same_authenticated_context(&other.context))
    }

    pub(crate) fn issue_ordinary_output(
        producer: &[u8],
        admission: &crate::artifacts::SourceEvidenceAdmission<'_>,
        issued: &crate::artifacts::AdmittedNativeTargetProducts,
    ) -> Result<Arc<Self>, CompileError> {
        let output = issued.output();
        if admission.exact().is_some() {
            return Err(failure(
                "ordinary execution evidence received exact source admission",
            ));
        }
        let evidence = admission.completed();
        let generated = evidence
            .modules
            .iter()
            .filter(|row| !row.boot && row.source == Path::new(crate::cache::GENERATED_SOURCE))
            .map(|row| identity(&row.unit, &row.module))
            .collect::<Vec<_>>();
        let [original_target] = generated.as_slice() else {
            return Err(failure(
                "ordinary execution evidence lacks its unique generated target",
            ));
        };
        let required = evidence
            .modules
            .iter()
            .filter(|row| !row.boot)
            .map(|row| identity(&row.unit, &row.module))
            .collect::<Vec<_>>();
        ExactDeclarationContext::prepare_issued_output(
            CanonicalProducerIdentity::from_producer_bytes(producer).sha256(),
            output,
            admission,
            original_target.clone(),
            &required,
        )?
        .finalize_native_target(issued)
    }
}
impl PartialEq for OriginalExecutionEvidence {
    fn eq(&self, other: &Self) -> bool {
        self.same_authenticated_execution(other)
    }
}
impl Eq for OriginalExecutionEvidence {}

impl<'a> CompletedOriginalExecution<'a> {
    pub fn evidence(&self) -> &'a Arc<OriginalExecutionEvidence> {
        self.evidence
    }
    pub fn context(&self) -> &'a Arc<ExactDeclarationContext> {
        self.evidence.context()
    }
    pub fn target(&self) -> &'a ExactModuleIdentity {
        self.evidence.target()
    }
    pub(crate) fn original_preview_interface_graph(
        &self,
    ) -> Result<BTreeMap<ExactModuleIdentity, TemplateInterfaceNode>, CompileError> {
        self.context()
            .preview_interface_graph_for_target(self.target())
    }
    pub(crate) fn original_display_projection(
        &self,
        input: &super::SelectedTypeInterfaces,
    ) -> Result<ExactDeclarationContext, CompileError> {
        self.context()
            .display_projection_for_target(input, self.target())
    }
}

/// Private assembly under the actual ordinary or exact output issuer. This
/// draft cannot escape as completed evidence before native target finalization.
pub(super) struct OriginalExecutionDraft {
    pub(super) context: ExactDeclarationContext,
    target: ExactModuleIdentity,
    census: OriginalInstanceCensus,
}
impl OriginalExecutionDraft {
    pub(super) fn new(
        context: ExactDeclarationContext,
        target: ExactModuleIdentity,
        missing: Vec<ExactModuleIdentity>,
    ) -> Self {
        let missing = missing.into_iter().collect::<BTreeSet<_>>();
        let census = if missing.is_empty() {
            OriginalInstanceCensus::Complete
        } else {
            OriginalInstanceCensus::MissingOwners(missing.into_iter().collect::<Vec<_>>().into())
        };
        Self {
            context,
            target,
            census,
        }
    }
    #[cfg(test)]
    pub(super) fn missing_owners(&self) -> &[ExactModuleIdentity] {
        match &self.census {
            OriginalInstanceCensus::Complete => &[],
            OriginalInstanceCensus::MissingOwners(owners) => owners,
        }
    }

    pub(super) fn finalize_native_target(
        self,
        issued: &crate::artifacts::AdmittedNativeTargetProducts,
    ) -> Result<Arc<OriginalExecutionEvidence>, CompileError> {
        let output = issued.output();
        let target = issued.target();
        if !target.matches_original_output(output) {
            return Err(failure(
                "original execution target belongs to another physical compiler output",
            ));
        }
        let completed = target.linked_native_target().ok_or_else(|| {
            failure("original execution target lacks its issued native completion")
        })?;
        let context = Arc::new(
            self.context
                .retain_native_target_custody(target)?
                .retain_linked_native_target(completed)?,
        );
        target
            .resolve()?
            .ensure_original_execution_custody(&context)?;
        Ok(Arc::new(OriginalExecutionEvidence {
            context,
            target: self.target,
            census: self.census,
            identity: OnceLock::new(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_census_canonicalizes_missing_obligations_without_issuing_evidence() {
        let a = identity("fixture", "A");
        let z = identity("fixture", "Z");
        let draft = OriginalExecutionDraft::new(
            ExactDeclarationContext::empty(),
            identity("fixture", "Target"),
            vec![z.clone(), a.clone(), z.clone()],
        );
        assert_eq!(draft.missing_owners(), &[a, z]);
        assert_eq!(draft.context, ExactDeclarationContext::empty());
    }

    #[test]
    fn incomplete_error_retains_typed_target_and_census_through_classification() {
        let target = identity("fixture", "Target");
        let missing = vec![identity("fixture", "Missing")];
        let error = CompileError::from(IncompleteOriginalExecution {
            target: target.clone(),
            missing_owners: missing.clone().into(),
        });
        assert!(
            matches!(&error, CompileError::IncompleteOriginalExecution(cause)
            if cause.target() == &target && cause.missing_owners() == missing)
        );
        let envelope = crate::failclass::classify_compile(&error);
        assert!(
            matches!(envelope.cause, Some(crate::failclass::CompileFailureCause::IncompleteOriginalExecution {
            target: actual, missing_owners,
        }) if actual == target && missing_owners == missing)
        );
    }
}
