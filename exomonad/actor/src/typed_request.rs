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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_signatures_preserve_input_progress_and_raw_reply_roles() {
        let sites = crate::request::test_support::request_signature_sites();
        let requests = sites.iter().collect::<Vec<_>>();
        assert_eq!(
            requests.len(),
            2,
            "both actual authored helpers issue requests"
        );
        for site in requests {
            let layout = site.request_input_layout().unwrap();
            let signature = decode_typed_request_site(site.site as i64, Some(site)).unwrap();
            assert_eq!(signature.response.expected_type(), "Int");
            let progress = layout.signatures().progress().is_some();
            assert_eq!(
                signature.input_type,
                if progress { "Ordering" } else { "Bool" }
            );
            assert_eq!(
                signature.response.progress_type.as_deref(),
                if progress { Some("Maybe Bool") } else { None }
            );
            assert_eq!(layout.response_index(), if progress { 2 } else { 1 });
            assert!(layout
                .response()
                .signature()
                .names()
                .iter()
                .any(|name| name.module() == "Tidepool.Agent.Reply.Internal"
                    && name.occurrence() == "ResponseResult"));

            // Presentation edits cannot change compiler-issued role selection.
            let mut renamed = site.clone();
            renamed.ty = "unrelated displayed answer".into();
            for input in &mut renamed.inputs {
                input.ty = "unrelated displayed live input".into();
            }
            assert_eq!(
                decode_typed_request_site(site.site as i64, Some(&renamed)).unwrap(),
                signature
            );

            let mut malformed = site.clone();
            malformed.request_type_signatures = None;
            assert!(matches!(
                decode_typed_request_site(site.site as i64, Some(&malformed)),
                Err(RequestSignatureError::Layout(
                    RequestInputLayoutError::MissingSignatures { .. }
                ))
            ));
            for count in 0..=4 {
                if count == site.inputs.len() {
                    continue;
                }
                let mut malformed = site.clone();
                malformed.inputs.resize(count, site.inputs[0].clone());
                assert!(matches!(malformed.request_input_layout(),
                    Err(RequestInputLayoutError::InputArity { actual, expected, .. })
                        if actual == count && expected == site.inputs.len()));
            }
            let mut malformed = site.clone();
            malformed.input_type_witnesses.pop();
            assert!(matches!(
                malformed.request_input_layout(),
                Err(RequestInputLayoutError::WitnessArity { .. })
            ));
            for index in 0..site.inputs.len() {
                let mut malformed = site.clone();
                malformed.input_type_witnesses[index] = None;
                assert!(matches!(malformed.request_input_layout(),
                    Err(RequestInputLayoutError::MissingWitness { index: missing, .. }) if missing == index));
            }
        }
        assert!(matches!(
            decode_typed_request_site(-1, None),
            Err(RequestSignatureError::InvalidSite(-1))
        ));
        assert!(matches!(
            decode_typed_request_site(17, None),
            Err(RequestSignatureError::MissingSite(17))
        ));
    }
}
