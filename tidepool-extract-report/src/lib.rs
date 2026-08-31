//! Typed stdout protocol emitted by the Tidepool Haskell compiler worker.
//!
//! This crate owns only the wire schema and strict decoding. Compilation
//! policy, diagnostic rendering, and process-status interpretation belong to
//! `tidepool-toolchain`; the procedural macro also consumes this schema but
//! deliberately retains its own old-binary fallback policy.

/// The diagnostics report version understood by this build.
pub const REPORT_VERSION: u32 = 2;

/// The result of one accepted compiler-worker request.
#[derive(Clone, Copy, Debug, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ExtractOutcome {
    Success,
    SourceFailure,
    WorkerFailure,
}

/// Severity assigned by GHC (or by the worker for an unspanned exception).
#[derive(Clone, Copy, Debug, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

impl std::fmt::Display for DiagnosticSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error => f.write_str("error"),
            Self::Warning => f.write_str("warning"),
        }
    }
}

/// A concrete source span attached to a worker diagnostic.
#[derive(Clone, Debug, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticSpan {
    pub file: String,
    #[serde(rename = "startLine")]
    pub start_line: u32,
    #[serde(rename = "startCol")]
    pub start_col: u32,
    #[serde(rename = "endLine")]
    pub end_line: u32,
    #[serde(rename = "endCol")]
    pub end_col: u32,
}

/// One structured compiler-worker diagnostic.
#[derive(Clone, Debug, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtractDiagnostic {
    /// `None` when GHC supplied an `UnhelpfulSpan`, or for worker failures.
    pub span: Option<DiagnosticSpan>,
    pub severity: DiagnosticSeverity,
    pub message: String,
}

/// A validated V2 response from one accepted worker request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractReport {
    pub outcome: ExtractOutcome,
    pub diagnostics: Vec<ExtractDiagnostic>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReport {
    #[serde(rename = "version")]
    _version: u32,
    outcome: ExtractOutcome,
    diagnostics: Vec<ExtractDiagnostic>,
}

/// A response that cannot be decoded under the current worker-report schema.
#[derive(Debug, thiserror::Error)]
pub enum ReportDecodeError {
    #[error("response is not valid JSON: {0}")]
    MalformedJson(serde_json::Error),
    #[error("response has no unsigned integer `version` field")]
    MissingVersion,
    #[error("unsupported response version {seen}; this build expects {expected}")]
    UnsupportedVersion { seen: u64, expected: u32 },
    #[error("response does not match the V2 product shape: {0}")]
    InvalidShape(serde_json::Error),
}

/// Strictly decode one complete worker stdout response.
///
/// The version is inspected before the V2 product shape so a future schema
/// reports version skew rather than an incidental missing/unknown field.
pub fn decode_report(bytes: &[u8]) -> Result<ExtractReport, ReportDecodeError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(ReportDecodeError::MalformedJson)?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or(ReportDecodeError::MissingVersion)?;
    if version != u64::from(REPORT_VERSION) {
        return Err(ReportDecodeError::UnsupportedVersion {
            seen: version,
            expected: REPORT_VERSION,
        });
    }
    let report: WireReport =
        serde_json::from_value(value).map_err(ReportDecodeError::InvalidShape)?;
    Ok(ExtractReport {
        outcome: report.outcome,
        diagnostics: report.diagnostics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_each_outcome_and_severity() {
        for (wire, expected) in [
            ("success", ExtractOutcome::Success),
            ("source-failure", ExtractOutcome::SourceFailure),
            ("worker-failure", ExtractOutcome::WorkerFailure),
        ] {
            let json = format!(
                r#"{{"version":2,"outcome":"{wire}","diagnostics":[{{"span":null,"severity":"error","message":"boom"}},{{"span":null,"severity":"warning","message":"careful"}}]}}"#
            );
            let report = decode_report(json.as_bytes()).unwrap();
            assert_eq!(report.outcome, expected);
            assert_eq!(report.diagnostics[0].severity, DiagnosticSeverity::Error);
            assert_eq!(report.diagnostics[1].severity, DiagnosticSeverity::Warning);
        }
    }

    #[test]
    fn preserves_unicode_diagnostics() {
        let report = decode_report(
            br#"{"version":2,"outcome":"worker-failure","diagnostics":[{"span":null,"severity":"error","message":"caf\u00e9 \ud83c\udf0a"}]}"#,
        )
        .unwrap();
        assert_eq!(report.diagnostics[0].message, "café 🌊");
    }

    #[test]
    fn rejects_version_shape_and_enum_drift() {
        assert!(matches!(
            decode_report(br#"{"version":99,"anything":true}"#),
            Err(ReportDecodeError::UnsupportedVersion { seen: 99, .. })
        ));
        assert!(matches!(
            decode_report(br#"{"outcome":"success","diagnostics":[]}"#),
            Err(ReportDecodeError::MissingVersion)
        ));
        assert!(decode_report(br#"{"version":2,"outcome":"mystery","diagnostics":[]}"#).is_err());
        assert!(
            decode_report(br#"{"version":2,"outcome":"success","diagnostics":[],"extra":1}"#)
                .is_err()
        );
        assert!(decode_report(
            br#"{"version":2,"outcome":"success","diagnostics":[{"span":null,"severity":"fatal","message":"x"}]}"#
        )
        .is_err());
    }
}
