//! Pure observation of an already mounted original input. The compiler proof
//! certifies a preview target, never a binding or an authored cell completion.
use std::path::Path;
use std::sync::Arc;

use ciborium::value::Value;
use tidepool_repr::execution_schema::PreparedProgram;
use tidepool_repr::DataConTable;

use crate::checked_cell::{
    array, decode, encode_signature, hash, hex, read, read_table, row, string, text,
    CanonicalInputTypeWitness, CheckedValueInputs, ExactHostBindingInterface,
};
use crate::declaration_context::ExactDeclarationContext;
use crate::CompileError;

#[derive(Clone, Debug)]
pub struct ActivationPreviewSpecification {
    pub admission_digest: [u8; 32],
    pub generation: u64,
    pub budget: u64,
    pub template_source: String,
}

/// Missing original code is a distinct result, not evidence of instance absence.
pub enum ActivationPreviewSelection {
    Ready(crate::artifacts::ModuleCandidateOffer),
    OriginalDisplayEvidenceUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationPreviewDisposition {
    Rendered,
    Opaque,
}

pub(crate) struct ActivationPreviewOffer {
    pub(crate) specification: ActivationPreviewSpecification,
    pub(crate) input: Arc<ExactHostBindingInterface>,
    pub(crate) values: Arc<CheckedValueInputs>,
    pub(crate) original_execution: Arc<ExactDeclarationContext>,
}

impl ActivationPreviewOffer {
    pub(crate) fn validate(&self) -> Result<(), CompileError> {
        let spec = &self.specification;
        if spec.admission_digest == [0; 32]
            || spec.generation == 0
            || spec.generation == self.input.generation()
            || spec.template_source.len() > 32 << 20
            || spec
                .template_source
                .matches("{{ACTIVATION_PREVIEW}}")
                .count()
                != 1
            || spec
                .template_source
                .matches("TidepoolActivationInput")
                .count()
                != 1
            || spec.template_source.contains("{{TURN_STMT}}")
            || spec.template_source.contains("{{BINDERS}}")
            || self.input.prototype().original_input_type().is_none()
            || string(&row(self.input.binder(), 7)?[0])? != "sessionInput"
        {
            return Err(failure(
                "pure activation preview has another input or recipe",
            ));
        }
        Ok(())
    }

    pub(crate) fn authorization(&self, original_interfaces: Value) -> Result<Value, CompileError> {
        self.validate()?;
        let witness = self
            .input
            .prototype()
            .original_input_type()
            .expect("validated live input");
        let baseline = self.values.baseline_authorization();
        let baseline = row(&baseline, 1)?;
        Ok(array([
            text(crate::artifacts::CheckedPurpose::ActivationPreview.wire_tag()),
            text(hex(&self.specification.admission_digest)),
            Value::Integer(self.specification.generation.into()),
            Value::Integer(self.specification.budget.into()),
            text(hash(self.specification.template_source.as_bytes())),
            Value::Integer(self.input.generation().into()),
            self.input.binder().clone(),
            encode_signature(witness.signature()),
            Value::Bytes(witness.original_bytes().to_vec()),
            {
                let fields = row(&baseline[0], 4)?;
                let certificate = self.input.value_interface_certificate();
                array([
                    fields[1].clone(),
                    fields[2].clone(),
                    fields[3].clone(),
                    text(hash(
                        certificate
                            .certified_interface()
                            .interface()
                            .package_imports_bytes(),
                    )),
                ])
            },
            original_interfaces,
            {
                let target = self.original_execution.original_instance_target()?;
                array([text(&target.unit), text(&target.module)])
            },
        ]))
    }

    pub(crate) fn unavailable(&self, root: &Path, request: &str) -> Result<bool, CompileError> {
        let path = root.join("activation-preview-unavailable.cbor");
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        self.validate()?;
        let receipt = decode(&read(&path, 4 << 20)?)?;
        let fields = row(&receipt, 7)?;
        let witness = self
            .input
            .prototype()
            .original_input_type()
            .expect("validated input");
        if string(&fields[0])? != "TPEXACTACTIVATIONPREVIEWUNAVAILABLE1"
            || string(&fields[1])? != "1"
            || string(&fields[2])? != request
            || string(&fields[3])? != hex(&self.specification.admission_digest)
            || fields[4] != Value::Integer(self.specification.generation.into())
            || string(&fields[5])? != hash(self.specification.template_source.as_bytes())
            || fields[6] != Value::Bytes(witness.original_bytes().to_vec())
            || root.join("turn.cbor").exists()
        {
            return Err(failure(
                "unavailable activation preview differs from its original offer",
            ));
        }
        Ok(true)
    }

    pub(crate) fn seal(
        &self,
        root: &Path,
        request: &str,
        source: &str,
        target: &Arc<PreparedProgram>,
        artifact_context: &Arc<ExactDeclarationContext>,
    ) -> Result<Arc<ExactCompiledActivationPreview>, CompileError> {
        self.validate()?;
        let receipt = decode(&read(root.join("activation-preview.cbor"), 4 << 20)?)?;
        let fields = row(&receipt, 8)?;
        if string(&fields[0])? != "TPEXACTACTIVATIONPREVIEW1"
            || string(&fields[1])? != "1"
            || string(&fields[2])? != request
            || string(&fields[3])? != hex(&self.specification.admission_digest)
            || fields[4] != Value::Integer(self.specification.generation.into())
            || string(&fields[5])? != hash(source.as_bytes())
        {
            return Err(failure(
                "activation preview receipt differs from its protected offer",
            ));
        }
        let Value::Bytes(bytes) = &fields[6] else {
            return Err(failure(
                "activation preview canonical input witness is absent",
            ));
        };
        let witness = CanonicalInputTypeWitness::from_bytes(bytes)?;
        let original = self
            .input
            .prototype()
            .original_input_type()
            .expect("validated live input");
        if &witness != original || witness.signature().names() != original.signature().names() {
            return Err(failure(
                "activation preview changed the original input type",
            ));
        }
        let disposition = decode_disposition(&fields[7])?;
        let turn = decode(&read(root.join("turn.cbor"), 32 << 20)?)?;
        let turn = row(&turn, 2)?;
        let expression = row(&turn[1], 3)?;
        if string(&turn[0])? != "Expr" || string(&expression[2])? != source {
            return Err(failure(
                "activation preview exported a binding or changed its recipe",
            ));
        }
        let sites = crate::turn_observations::decode_turn_yield_sites(&expression[1])?;
        if !sites.is_empty() {
            return Err(failure("pure activation preview emitted suspension sites"));
        }
        let producer = self.input.prototype().producer();
        Ok(Arc::new(ExactCompiledActivationPreview {
            input: self.input.clone(),
            specification: self.specification.clone(),
            target: target.clone(),
            table: read_table(root)?,
            yield_sites_digest: crate::artifacts::yield_sites_metadata_digest(&sites)?,
            disposition,
            original_interfaces: Arc::new(ExactDeclarationContext::from_authenticated_interfaces(
                producer,
                artifact_context.artifact_view(),
            )?),
            original_execution: self.original_execution.clone(),
        }))
    }
}

/// The original compiler proof retains the complete admitted interface
/// environment. Native execution is demanded later by the selected preview;
/// a nominal input type does not require its owner's native code by itself.
pub(crate) fn validate_original_display_context(
    input: &ExactHostBindingInterface,
    context: &ExactDeclarationContext,
) -> Result<bool, CompileError> {
    let prototype = input.prototype();
    if prototype.producer() != context.toolchain_identity_sha256() {
        return Err(failure(
            "activation preview has another original compiler producer",
        ));
    }
    let _ = context
        .clone()
        .extend_interface_context(prototype.context())?;
    if prototype.original_input_type().is_none() {
        return Err(failure(
            "activation preview requires original live input authority",
        ));
    }
    Ok(matches!(
        context.original_instance_environment(),
        crate::declaration_context::OriginalInstanceEnvironment::Complete { .. }
    ))
}

/// Authored instance implementations are retained native originals, rather
/// than source originals that the compiler may lower again from Core. Offer
/// only carriers matching the original request's selected canonical interface.
/// Their complete census adds availability, never lexical or heap authority.
pub(crate) fn original_native_declaration_inputs(
    context: &ExactDeclarationContext,
    producer: &[u8],
) -> Result<Option<crate::declaration_context::OriginalCompilerInputs>, CompileError> {
    use crate::artifact_inventory::{ArtifactPayload, CanonicalProducerIdentity};
    use crate::certified_products::CanonicalOrigin;

    context.original_instance_target()?;
    let selected = context.compiler_metadata_snapshot()?;
    let products = context
        .artifact_view()
        .entries()
        .into_iter()
        .filter_map(|entry| {
            let ArtifactPayload::Original(product) = &entry.payload else {
                return None;
            };
            let interface = product.module_interface()?;
            if !matches!(
                interface.origin(),
                CanonicalOrigin::NativeAuthoredDeclaration { .. }
            ) {
                return None;
            }
            let selected = selected.entries.get(&entry.descriptor.owner)?;
            match &selected.payload {
                ArtifactPayload::Canonical(canonical) if canonical == interface => {
                    Some(product.clone())
                }
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    if products.is_empty() {
        return Ok(None);
    }
    crate::declaration_context::OriginalCompilerInputs::from_native_availability(
        context,
        CanonicalProducerIdentity::from_producer_bytes(producer),
        &products,
    )
    .map(Some)
}

#[derive(Debug)]
pub struct ExactCompiledActivationPreview {
    input: Arc<ExactHostBindingInterface>,
    specification: ActivationPreviewSpecification,
    target: Arc<PreparedProgram>,
    table: DataConTable,
    yield_sites_digest: [u8; 32],
    disposition: ActivationPreviewDisposition,
    original_interfaces: Arc<ExactDeclarationContext>,
    original_execution: Arc<ExactDeclarationContext>,
}

impl ExactCompiledActivationPreview {
    pub fn input_interface(&self) -> &Arc<ExactHostBindingInterface> {
        &self.input
    }
    pub fn admission_digest(&self) -> [u8; 32] {
        self.specification.admission_digest
    }
    pub fn generation(&self) -> u64 {
        self.specification.generation
    }
    pub fn disposition(&self) -> ActivationPreviewDisposition {
        self.disposition
    }
    pub fn target_owned(&self) -> Arc<PreparedProgram> {
        self.target.clone()
    }
    pub fn matches_target(&self, target: &PreparedProgram) -> bool {
        std::ptr::eq(self.target.as_ref(), target) || self.target.as_ref() == target
    }
    pub fn validate_table(&self, table: &DataConTable) -> Result<(), CompileError> {
        if table != &self.table {
            return Err(failure("activation preview constructor metadata changed"));
        }
        Ok(())
    }
    pub fn validate_yield_sites(&self, sites: &[crate::YieldSite]) -> Result<(), CompileError> {
        if crate::artifacts::yield_sites_metadata_digest(sites)? != self.yield_sites_digest {
            return Err(failure("activation preview typed-site metadata changed"));
        }
        Ok(())
    }
    pub fn original_interface_context(
        &self,
        target: &PreparedProgram,
        table: &DataConTable,
        sites: &[crate::YieldSite],
    ) -> Result<Arc<ExactDeclarationContext>, CompileError> {
        if !self.matches_target(target) {
            return Err(failure(
                "activation preview belongs to another prepared target",
            ));
        }
        self.validate_table(table)?;
        self.validate_yield_sites(sites)?;
        Ok(self.original_interfaces.clone())
    }
    pub fn original_execution_context(
        &self,
        target: &PreparedProgram,
        table: &DataConTable,
        sites: &[crate::YieldSite],
    ) -> Result<Arc<ExactDeclarationContext>, CompileError> {
        self.original_interface_context(target, table, sites)?;
        Ok(self.original_execution.clone())
    }
}

fn decode_disposition(value: &Value) -> Result<ActivationPreviewDisposition, CompileError> {
    match string(value)? {
        "rendered" => Ok(ActivationPreviewDisposition::Rendered),
        "opaque" => Ok(ActivationPreviewDisposition::Opaque),
        _ => Err(failure("activation preview disposition is unknown")),
    }
}
fn failure(message: &str) -> CompileError {
    CompileError::ExtractFailed(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_disposition_does_not_accept_missing_evidence_or_infrastructure_as_opaque() {
        assert_eq!(
            decode_disposition(&text("rendered")).unwrap(),
            ActivationPreviewDisposition::Rendered
        );
        assert_eq!(
            decode_disposition(&text("opaque")).unwrap(),
            ActivationPreviewDisposition::Opaque
        );
        for refused in [
            "original-display-evidence-unavailable",
            "worker-failure",
            "budget",
            "",
            "Opaque",
        ] {
            assert!(decode_disposition(&text(refused)).is_err());
        }
    }
}

#[cfg(test)]
#[path = "activation_preview/native_declaration_input_tests.rs"]
mod native_declaration_input_tests;
