use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

pub(crate) const WORKER_REQUEST_FLAG: &str = "--worker-request-v1";
const MAGIC: &[u8; 8] = b"TPREQ001";

#[derive(Clone, Debug)]
enum Field {
    Input(OsString),
    OutputDir(OsString),
    Target(OsString),
    Targets(Vec<String>),
    Include(OsString),
    Turn,
    TurnTemplate { kind: String, path: PathBuf },
    TurnOut(OsString),
    TurnVerdict(OsString),
    Classify,
    ClassifyOut(OsString),
    BuildProductsDir(OsString),
    SessionRoot(OsString),
    InjectVal(OsString),
    SessionBind,
    BindName(OsString),
    BindGen(u64),
    EmitBoundBinders(OsString),
    ProbeOnly,
}

/// A versioned, typed request for the Haskell compiler worker.
///
/// Fields retain their domain shape until encoding. `legacy_argv` exists only
/// for cache keys and transitional launcher compatibility; worker dispatch
/// consumes [`encode`](Self::encode), not reparsed command-line flags.
#[derive(Clone, Debug, Default)]
pub struct ExtractRequest {
    fields: Vec<Field>,
}

impl ExtractRequest {
    pub(crate) fn input(&mut self, value: impl AsRef<OsStr>) {
        self.fields.push(Field::Input(value.as_ref().to_owned()));
    }

    pub(crate) fn output_dir(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::OutputDir(value.as_ref().to_owned()));
    }

    pub(crate) fn target(&mut self, value: impl AsRef<OsStr>) {
        self.fields.push(Field::Target(value.as_ref().to_owned()));
    }

    pub(crate) fn targets<I, S>(&mut self, values: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.fields.push(Field::Targets(
            values
                .into_iter()
                .map(|value| value.as_ref().to_owned())
                .collect(),
        ));
    }

    pub(crate) fn include(&mut self, value: impl AsRef<OsStr>) {
        self.fields.push(Field::Include(value.as_ref().to_owned()));
    }

    pub(crate) fn turn(&mut self) {
        self.fields.push(Field::Turn);
    }

    pub(crate) fn turn_template(&mut self, kind: &str, path: &Path) {
        self.fields.push(Field::TurnTemplate {
            kind: kind.to_owned(),
            path: path.to_owned(),
        });
    }

    pub(crate) fn turn_out(&mut self, value: impl AsRef<OsStr>) {
        self.fields.push(Field::TurnOut(value.as_ref().to_owned()));
    }

    pub(crate) fn turn_verdict(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::TurnVerdict(value.as_ref().to_owned()));
    }

    pub(crate) fn classify(&mut self) {
        self.fields.push(Field::Classify);
    }

    pub(crate) fn classify_out(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::ClassifyOut(value.as_ref().to_owned()));
    }

    pub(crate) fn build_products_dir(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::BuildProductsDir(value.as_ref().to_owned()));
    }

    pub(crate) fn session_root(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::SessionRoot(value.as_ref().to_owned()));
    }

    pub(crate) fn inject_val(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::InjectVal(value.as_ref().to_owned()));
    }

    pub(crate) fn session_bind(&mut self) {
        self.fields.push(Field::SessionBind);
    }

    pub(crate) fn bind_name(&mut self, value: impl AsRef<OsStr>) {
        self.fields.push(Field::BindName(value.as_ref().to_owned()));
    }

    pub(crate) fn bind_gen(&mut self, value: u64) {
        self.fields.push(Field::BindGen(value));
    }

    pub(crate) fn emit_bound_binders(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::EmitBoundBinders(value.as_ref().to_owned()));
    }

    pub(crate) fn probe_only(&mut self) {
        self.fields.push(Field::ProbeOnly);
    }

    pub(crate) fn legacy_argv(&self) -> Vec<OsString> {
        let mut inputs = Vec::new();
        let mut flags = Vec::new();
        for field in &self.fields {
            match field {
                Field::Input(value) => inputs.push(value.clone()),
                Field::OutputDir(value) => flag(&mut flags, "--output-dir", value),
                Field::Target(value) => flag(&mut flags, "--target", value),
                Field::Targets(values) => {
                    flag(&mut flags, "--targets", OsStr::new(&values.join(",")))
                }
                Field::Include(value) => flag(&mut flags, "--include", value),
                Field::Turn => flags.push("--turn".into()),
                Field::TurnTemplate { kind, path } => flag(
                    &mut flags,
                    "--turn-template",
                    OsStr::new(&format!("{kind}={}", path.display())),
                ),
                Field::TurnOut(value) => flag(&mut flags, "--turn-out", value),
                Field::TurnVerdict(value) => flag(&mut flags, "--turn-verdict", value),
                Field::Classify => flags.push("--classify".into()),
                Field::ClassifyOut(value) => flag(&mut flags, "--classify-out", value),
                Field::BuildProductsDir(value) => flag(&mut flags, "--build-products-dir", value),
                Field::SessionRoot(value) => flag(&mut flags, "--session-root", value),
                Field::InjectVal(value) => flag(&mut flags, "--inject-val", value),
                Field::SessionBind => flags.push("--session-bind".into()),
                Field::BindName(value) => flag(&mut flags, "--bind-name", value),
                Field::BindGen(value) => {
                    flag(&mut flags, "--bind-gen", OsStr::new(&value.to_string()))
                }
                Field::EmitBoundBinders(value) => flag(&mut flags, "--emit-bound-binders", value),
                Field::ProbeOnly => flags.push("--probe-only".into()),
            }
        }
        inputs.extend(flags);
        inputs
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        push_u32(&mut out, self.fields.len());
        for field in &self.fields {
            encode_field(&mut out, field);
        }
        out
    }

    pub(crate) fn worker_argv(&self) -> Vec<OsString> {
        let mut argv = self.legacy_argv();
        argv.push(WORKER_REQUEST_FLAG.into());
        argv.push(hex(&self.encode()).into());
        argv
    }
}

fn flag(out: &mut Vec<OsString>, name: &str, value: &OsStr) {
    out.push(name.into());
    out.push(value.to_owned());
}

fn push_u32(out: &mut Vec<u8>, value: usize) {
    assert!(
        u32::try_from(value).is_ok(),
        "extract request exceeds u32 field limit"
    );
    let value = value as u32;
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_frame(out: &mut Vec<u8>, value: &OsStr) {
    let value = value.to_string_lossy();
    push_u32(out, value.len());
    out.extend_from_slice(value.as_bytes());
}

fn encode_field(out: &mut Vec<u8>, field: &Field) {
    match field {
        Field::Input(value) => tagged_frame(out, 1, value),
        Field::OutputDir(value) => tagged_frame(out, 2, value),
        Field::Target(value) => tagged_frame(out, 3, value),
        Field::Targets(values) => {
            out.push(4);
            push_u32(out, values.len());
            for value in values {
                push_frame(out, OsStr::new(value));
            }
        }
        Field::Include(value) => tagged_frame(out, 8, value),
        Field::Turn => out.push(16),
        Field::TurnTemplate { kind, path } => {
            out.push(17);
            push_frame(out, OsStr::new(kind));
            push_frame(out, path.as_os_str());
        }
        Field::TurnOut(value) => tagged_frame(out, 18, value),
        Field::TurnVerdict(value) => tagged_frame(out, 19, value),
        Field::Classify => out.push(20),
        Field::ClassifyOut(value) => tagged_frame(out, 21, value),
        Field::BuildProductsDir(value) => tagged_frame(out, 25, value),
        Field::SessionRoot(value) => tagged_frame(out, 12, value),
        Field::InjectVal(value) => tagged_frame(out, 13, value),
        Field::SessionBind => out.push(9),
        Field::BindName(value) => tagged_frame(out, 10, value),
        Field::BindGen(value) => {
            out.push(11);
            out.extend_from_slice(&value.to_le_bytes());
        }
        Field::EmitBoundBinders(value) => tagged_frame(out, 14, value),
        Field::ProbeOnly => out.push(15),
    }
}

fn tagged_frame(out: &mut Vec<u8>, tag: u8, value: &OsStr) {
    out.push(tag);
    push_frame(out, value);
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_has_versioned_header_and_typed_integer() {
        let mut request = ExtractRequest::default();
        request.input("Expr.hs");
        request.bind_gen(0x0102_0304_0506_0708);
        let bytes = request.encode();
        assert_eq!(&bytes[..8], MAGIC);
        assert_eq!(&bytes[8..12], &2u32.to_le_bytes());
        assert_eq!(bytes[12], 1);
        assert_eq!(bytes[24], 11);
        assert_eq!(&bytes[25..33], &0x0102_0304_0506_0708u64.to_le_bytes());
    }
}
