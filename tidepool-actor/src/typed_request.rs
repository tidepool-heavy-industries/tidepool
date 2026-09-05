//! Shared type metadata for requests presented to external actor applications.

use tidepool_runtime::YieldSite;

/// Model-facing description of one statically typed response obligation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseExpectation {
    expected_type: String,
    pub(crate) progress_type: Option<String>,
}

impl ResponseExpectation {
    pub(crate) fn respond_signature(&self, effects: &str) -> String {
        format!(
            "respond :: ({}) -> Eff {effects} TidepoolVoid.Void",
            self.expected_type
        )
    }

    #[must_use]
    pub(crate) fn new(expected_type: impl Into<String>) -> Self {
        Self {
            expected_type: expected_type.into(),
            progress_type: None,
        }
    }

    #[must_use]
    pub(crate) fn expected_type(&self) -> &str {
        &self.expected_type
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypedRequestSignature {
    pub(crate) input_type: String,
    pub(crate) input_modules: Vec<String>,
    pub(crate) response: ResponseExpectation,
    pub(crate) output_modules: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RequestSignatureError {
    #[error("request carried invalid site id {0}")]
    InvalidSite(i64),
    #[error("request site {0} is absent from its GHC metadata")]
    MissingSite(u64),
    #[error("request site {site} describes {actual} live input types, expected an input and optional progress type")]
    InputArity { site: u64, actual: usize },
}

pub(crate) fn decode_typed_request_site(
    site: i64,
    sites: &[YieldSite],
) -> Result<TypedRequestSignature, RequestSignatureError> {
    let site = u64::try_from(site).map_err(|_| RequestSignatureError::InvalidSite(site))?;
    let metadata = sites
        .iter()
        .find(|metadata| metadata.site == site)
        .ok_or(RequestSignatureError::MissingSite(site))?;
    let Some(input) = metadata
        .inputs
        .first()
        .filter(|_| metadata.inputs.len() <= 2)
    else {
        return Err(RequestSignatureError::InputArity {
            site,
            actual: metadata.inputs.len(),
        });
    };
    let mut response = ResponseExpectation::new(metadata.ty.clone());
    let mut output_modules = metadata.modules.clone();
    if let Some(progress) = metadata.inputs.get(1) {
        response.progress_type = Some(progress.ty.clone());
        output_modules.extend(progress.modules.iter().cloned());
    }
    Ok(TypedRequestSignature {
        input_type: input.ty.clone(),
        input_modules: input.modules.clone(),
        response,
        output_modules,
    })
}
