//! Shared type metadata for requests presented to external actor applications.

use tidepool_runtime::{RequestInputLayoutError, YieldSite};

/// Model-facing description of one statically typed response obligation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseExpectation {
    expected_type: String,
    pub(crate) declaration: Option<String>,
    /// Modules the reply type's own head 'TyCon' is defined in (not the
    /// progress type's), used to decide whether `declaration` is worth
    /// showing: a workspace- or session-declared type, versus a library or
    /// stdlib type the model already knows by name.
    pub(crate) declaration_modules: Vec<String>,
    pub(crate) progress_type: Option<String>,
}

impl ResponseExpectation {
    #[must_use]
    pub(crate) fn new(expected_type: impl Into<String>) -> Self {
        Self {
            expected_type: expected_type.into(),
            progress_type: None,
            declaration: None,
            declaration_modules: Vec::new(),
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
    pub(crate) response: ResponseExpectation,
}

#[derive(Debug, thiserror::Error)]
pub enum RequestSignatureError {
    #[error("request carried invalid site id {0}")]
    InvalidSite(i64),
    #[error("request site {0} is absent from its GHC metadata")]
    MissingSite(u64),
    #[error(transparent)]
    Layout(#[from] RequestInputLayoutError),
}

pub(crate) fn decode_typed_request_site(
    site: i64,
    metadata: Option<&YieldSite>,
) -> Result<TypedRequestSignature, RequestSignatureError> {
    let site = u64::try_from(site).map_err(|_| RequestSignatureError::InvalidSite(site))?;
    let metadata = metadata
        .filter(|metadata| metadata.site == site)
        .ok_or(RequestSignatureError::MissingSite(site))?;
    let layout = metadata.request_input_layout()?;
    let mut response = ResponseExpectation::new(layout.signatures().reply().presentation());
    response.declaration = metadata.reply_declaration.clone();
    response.declaration_modules = metadata.modules.clone();
    response.progress_type = layout
        .signatures()
        .progress()
        .map(|progress| progress.presentation().to_owned());
    Ok(TypedRequestSignature {
        input_type: layout.input().signature().presentation().to_owned(),
        response,
    })
}
