use super::*;
use crate::declaration_context::{
    ExactCompilationRequest, ExactCompileContext, ExactSourceAdmission,
};

/// An actual inspection and its request-owned source receipts. The isolated
/// runner owns every input and output until compiler settlement and case cleanup.
pub(crate) struct ExactSourceFixture {
    _directory: TempDir,
    applied: AppliedModuleCandidateOffer,
    compiler: tidepool_extract_cmd::CompilerIdentity,
    source_path: PathBuf,
    source: String,
    inspection_output: PathBuf,
    receipts: Vec<PathBuf>,
    admissions: Vec<ExactSourceAdmission>,
    runs: Vec<tidepool_extract_cmd::ExtractRun>,
}

impl ExactSourceFixture {
    pub(crate) fn inspect(
        context: Option<Arc<ExactCompileContext>>,
        source: &str,
        expression: &str,
    ) -> Result<Self, CompileError> {
        let directory = test_support::compiler_input_directory()?;
        let sources = directory.path().join("sources");
        let output = directory.path().join("output");
        std::fs::create_dir(&sources)?;
        std::fs::create_dir(&output)?;
        let source_path = sources.join("Consumer.hs");
        std::fs::write(&source_path, source)?;
        let include = vec![sources];
        let (endpoint, _) = crate::toolchain::bind_extract_endpoint()
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
        let offer = ModuleCandidateOffer::select_inspection(
            endpoint.identity().producer_bytes(),
            &include,
            &directory.path().join("request"),
            context,
            Vec::new(),
            &[],
        )?;
        let inspection_output = output.join("inspection.cbor");
        let mut command = ExtractCmd::new().map_err(|error| CompileError::Io(error.into()))?;
        command
            .input(&source_path)
            .includes(&include)
            .inspect_type(expression)
            .inspect_out(&inspection_output)
            .inspection_strict()
            .output_dir(&output);
        let applied = offer.apply_to(command, &output)?;
        let mut fixture = Self {
            _directory: directory,
            applied,
            compiler: endpoint.identity().clone(),
            source_path,
            source: source.to_owned(),
            inspection_output,
            receipts: Vec::new(),
            admissions: Vec::new(),
            runs: Vec::new(),
        };
        fixture.execute(endpoint)?;
        Ok(fixture)
    }

    /// Execute the same immutable issued request again. Both actual source
    /// snapshots remain authenticated by that request; no receipt is restamped.
    pub(crate) fn repeat_same_request(&mut self) -> Result<(), CompileError> {
        let previous = self
            .receipts
            .iter()
            .map(|path| {
                Ok((
                    path.clone(),
                    std::fs::read(path)?,
                    std::fs::read(path.parent().unwrap().join("source.hs"))?,
                ))
            })
            .collect::<Result<Vec<_>, std::io::Error>>()?;
        let (endpoint, _) = crate::toolchain::bind_extract_endpoint()
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
        if endpoint.identity() != &self.compiler {
            return Err(CompileError::ExtractFailed(
                "repeated exact-source inspection selected another compiler".into(),
            ));
        }
        self.execute(endpoint)?;
        assert_eq!(self.receipts.len(), previous.len() + 1);
        for (path, receipt, snapshot) in previous {
            assert_eq!(std::fs::read(&path)?, receipt);
            assert_eq!(
                std::fs::read(path.parent().unwrap().join("source.hs"))?,
                snapshot
            );
        }
        Ok(())
    }

    fn execute(
        &mut self,
        endpoint: tidepool_extract_cmd::CompilerEndpoint,
    ) -> Result<(), CompileError> {
        let applied = &self.applied;
        let run = test_support::with_settlement(|settlement| {
            let outcome = tidepool_extract_cmd::with_compiler_transaction(settlement, || {
                let run = CompileError::compiler_invocation_result(
                    endpoint.execute_with_input_files(
                        applied.command(),
                        applied.input_transport_files(),
                        |_| {},
                    ),
                )?;
                crate::diag::decode_extract_result(
                    run.success(),
                    &run.output.stdout,
                    &run.output.stderr,
                )?;
                Ok::<_, CompileError>(run)
            });
            outcome.action
        })?;
        assert!(!std::fs::read(&self.inspection_output)?.is_empty());
        let ordinal = self.runs.len();
        let output = self.inspection_output.parent().unwrap();
        std::fs::write(
            output.join(format!("inspection-{ordinal}.stdout")),
            &run.output.stdout,
        )?;
        std::fs::write(
            output.join(format!("inspection-{ordinal}.stderr")),
            &run.output.stderr,
        )?;
        std::fs::copy(
            &self.inspection_output,
            output.join(format!("inspection-{ordinal}.cbor")),
        )?;
        self.runs.push(run);
        self.admissions = self.request().validate_outputs(self.receipt_directory())?;
        self.receipts = std::fs::read_dir(self.receipt_directory().join(".exact-compilations"))?
            .map(|entry| entry.map(|entry| entry.path().join("receipt.cbor")))
            .collect::<Result<Vec<_>, _>>()?;
        self.receipts.sort();
        assert_eq!(self.admissions.len(), self.runs.len());
        assert_eq!(self.receipts.len(), self.runs.len());
        for admission in &self.admissions {
            admission.validate_request(self.request())?;
            assert_eq!(admission.witness().source_path(), self.source_path());
        }
        Ok(())
    }

    pub(crate) fn request(&self) -> &ExactCompilationRequest {
        self.applied
            .exact
            .as_ref()
            .expect("inspection issues an exact request")
    }

    pub(crate) fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    pub(crate) fn receipt_directory(&self) -> &Path {
        self.source_path.parent().unwrap()
    }

    pub(crate) fn receipt_paths(&self) -> &[PathBuf] {
        &self.receipts
    }

    pub(crate) fn admissions(&self) -> &[ExactSourceAdmission] {
        &self.admissions
    }
}
