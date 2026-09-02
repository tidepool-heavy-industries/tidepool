use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

pub(crate) const WORKER_REQUEST_FLAG: &str = "--worker-request-v4";
const MAGIC: &[u8; 8] = b"TPREQ004";

#[derive(Clone, Debug)]
enum Field {
    Input(OsString),
    OutputDir(OsString),
    Target(OsString),
    Targets(Vec<String>),
    DumpCore,
    AllClosed,
    TargetModuleOnly,
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
    BindGen(u64),
    HarnessProfile,
    InspectType(String),
    InspectInfo(String),
    InspectOut(OsString),
}

/// A versioned, typed request for the Haskell compiler worker.
///
/// Fields retain their domain shape until encoding. The CLI rendering exists
/// only for cache keys and diagnostics; worker dispatch consumes
/// [`encode`](Self::encode), not reparsed command-line flags.
#[derive(Clone, Debug, Default)]
pub struct ExtractRequest {
    fields: Vec<Field>,
}

impl ExtractRequest {
    /// Parse the human CLI into the same typed request used by
    /// library callers. Unknown options and missing/invalid values are usage
    /// errors; they are never reinterpreted as source paths.
    pub fn from_cli(args: &[OsString]) -> Result<Self, CliError> {
        let mut request = Self::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--output-dir") => request.output_dir(value(&mut args, "--output-dir")?),
                Some("--target") => request.target(value(&mut args, "--target")?),
                Some("--targets") => {
                    let raw = text_value(&mut args, "--targets")?;
                    request
                        .fields
                        .push(Field::Targets(raw.split(',').map(str::to_owned).collect()));
                }
                Some("--dump-core") => request.fields.push(Field::DumpCore),
                Some("--all-closed") => request.fields.push(Field::AllClosed),
                Some("--target-module-only") => request.fields.push(Field::TargetModuleOnly),
                Some("--include") => request.include(value(&mut args, "--include")?),
                Some("--turn") => request.turn(),
                Some("--turn-template") => {
                    let raw = text_value(&mut args, "--turn-template")?;
                    let Some((kind, path)) = raw.split_once('=') else {
                        return Err(CliError::new("--turn-template requires KIND=PATH"));
                    };
                    request.turn_template(kind, Path::new(path));
                }
                Some("--turn-out") => request.turn_out(value(&mut args, "--turn-out")?),
                Some("--turn-verdict") => request.turn_verdict(value(&mut args, "--turn-verdict")?),
                Some("--classify") => request.classify(),
                Some("--classify-out") => request.classify_out(value(&mut args, "--classify-out")?),
                Some("--build-products-dir") => {
                    request.build_products_dir(value(&mut args, "--build-products-dir")?)
                }
                Some("--session-root") => request.session_root(value(&mut args, "--session-root")?),
                Some("--inject-val") => request.inject_val(value(&mut args, "--inject-val")?),
                Some("--bind-gen") => {
                    let raw = text_value(&mut args, "--bind-gen")?;
                    let generation = raw
                        .parse()
                        .map_err(|_| CliError::new("--bind-gen requires an unsigned integer"))?;
                    request.bind_gen(generation);
                }
                Some("--harness-profile") => request.fields.push(Field::HarnessProfile),
                Some("--inspect-type") => request.fields.push(Field::InspectType(
                    text_value(&mut args, "--inspect-type")?.into(),
                )),
                Some("--inspect-info") => request.fields.push(Field::InspectInfo(
                    text_value(&mut args, "--inspect-info")?.into(),
                )),
                Some("--inspect-out") => request.inspect_out(value(&mut args, "--inspect-out")?),
                Some(option) if option.starts_with('-') => {
                    return Err(CliError::new(format!("unknown option: {option}")));
                }
                _ => request.input(arg),
            }
        }
        let has_input = request
            .fields
            .iter()
            .any(|field| matches!(field, Field::Input(_)));
        if !has_input {
            return Err(CliError::new("an input file is required"));
        }
        Ok(request)
    }

    /// Decode the versioned worker payload emitted by [`Self::encode`].
    ///
    /// This is the Rust-side protocol boundary for test workers and tooling
    /// that need to inspect a request rather than forward it opaquely. Invalid
    /// headers, truncated fields, unknown tags, and trailing bytes are errors.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let mut decoder = Decoder::new(bytes)?;
        let field_count = decoder.u32()? as usize;
        let mut fields = Vec::with_capacity(field_count);
        for _ in 0..field_count {
            let tag = decoder.byte()?;
            let field = match tag {
                1 => Field::Input(decoder.os_string()?),
                2 => Field::OutputDir(decoder.os_string()?),
                3 => Field::Target(decoder.os_string()?),
                4 => {
                    let count = decoder.u32()? as usize;
                    let mut values = Vec::with_capacity(count);
                    for _ in 0..count {
                        values.push(decoder.string()?);
                    }
                    Field::Targets(values)
                }
                5 => Field::DumpCore,
                6 => Field::AllClosed,
                7 => Field::TargetModuleOnly,
                8 => Field::Include(decoder.os_string()?),
                9 | 10 | 14 => return Err(ProtocolError::RetiredFieldTag(tag)),
                11 => Field::BindGen(decoder.u64()?),
                12 => Field::SessionRoot(decoder.os_string()?),
                13 => Field::InjectVal(decoder.os_string()?),
                16 => Field::Turn,
                17 => Field::TurnTemplate {
                    kind: decoder.string()?,
                    path: PathBuf::from(decoder.os_string()?),
                },
                18 => Field::TurnOut(decoder.os_string()?),
                19 => Field::TurnVerdict(decoder.os_string()?),
                20 => Field::Classify,
                21 => Field::ClassifyOut(decoder.os_string()?),
                24 => Field::HarnessProfile,
                25 => Field::BuildProductsDir(decoder.os_string()?),
                26 => Field::InspectType(decoder.string()?),
                27 => Field::InspectInfo(decoder.string()?),
                28 => Field::InspectOut(decoder.os_string()?),
                other => return Err(ProtocolError::UnknownFieldTag(other)),
            };
            fields.push(field);
        }
        decoder.finish()?;
        Ok(Self { fields })
    }

    /// Decode the complete argv accepted by the compiler worker.
    ///
    /// This is the inspection counterpart to the endpoint's private argv
    /// renderer. Wrappers and tests can assert on typed request fields without
    /// depending on the payload's hexadecimal transport representation.
    pub fn decode_worker_argv(args: &[OsString]) -> Result<Self, ProtocolError> {
        if args.len() != 2 || args[0] != WORKER_REQUEST_FLAG {
            return Err(ProtocolError::InvalidWorkerArgv);
        }
        let payload = args[1].to_str().ok_or(ProtocolError::NonUtf8Payload)?;
        Self::decode(&unhex(payload)?)
    }

    /// Requested output directory, if this request carries one.
    pub fn output_directory(&self) -> Option<&OsStr> {
        self.fields.iter().find_map(|field| match field {
            Field::OutputDir(value) => Some(value.as_os_str()),
            _ => None,
        })
    }

    /// Logical target names in request order.
    pub fn target_names(&self) -> Vec<String> {
        self.fields
            .iter()
            .flat_map(|field| match field {
                Field::Target(value) => vec![value.to_string_lossy().into_owned()],
                Field::Targets(values) => values.clone(),
                _ => Vec::new(),
            })
            .collect()
    }

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

    pub(crate) fn bind_gen(&mut self, value: u64) {
        self.fields.push(Field::BindGen(value));
    }

    pub(crate) fn inspect_type(&mut self, expression: &str) {
        self.fields.push(Field::InspectType(expression.to_owned()));
    }

    pub(crate) fn inspect_info(&mut self, name: &str) {
        self.fields.push(Field::InspectInfo(name.to_owned()));
    }

    pub(crate) fn inspect_out(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::InspectOut(value.as_ref().to_owned()));
    }

    pub(crate) fn cli_argv(&self) -> Vec<OsString> {
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
                Field::DumpCore => flags.push("--dump-core".into()),
                Field::AllClosed => flags.push("--all-closed".into()),
                Field::TargetModuleOnly => flags.push("--target-module-only".into()),
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
                Field::BindGen(value) => {
                    flag(&mut flags, "--bind-gen", OsStr::new(&value.to_string()))
                }
                Field::HarnessProfile => flags.push("--harness-profile".into()),
                Field::InspectType(value) => flag(&mut flags, "--inspect-type", OsStr::new(value)),
                Field::InspectInfo(value) => flag(&mut flags, "--inspect-info", OsStr::new(value)),
                Field::InspectOut(value) => flag(&mut flags, "--inspect-out", value),
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
        vec![WORKER_REQUEST_FLAG.into(), hex(&self.encode()).into()]
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, ProtocolError> {
        if bytes.get(..MAGIC.len()) != Some(MAGIC) {
            return Err(ProtocolError::InvalidHeader);
        }
        Ok(Self {
            bytes,
            cursor: MAGIC.len(),
        })
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], ProtocolError> {
        let end = self
            .cursor
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(ProtocolError::Truncated)?;
        let value = &self.bytes[self.cursor..end];
        self.cursor = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_le_bytes(self.fixed()?))
    }

    fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_le_bytes(self.fixed()?))
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], ProtocolError> {
        let mut value = [0; N];
        value.copy_from_slice(self.take(N)?);
        Ok(value)
    }

    fn frame(&mut self) -> Result<&'a [u8], ProtocolError> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    fn string(&mut self) -> Result<String, ProtocolError> {
        String::from_utf8(self.frame()?.to_vec()).map_err(|_| ProtocolError::NonUtf8Text)
    }

    fn os_string(&mut self) -> Result<OsString, ProtocolError> {
        Ok(self.string()?.into())
    }

    fn finish(self) -> Result<(), ProtocolError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(ProtocolError::TrailingBytes)
        }
    }
}

/// A malformed versioned worker request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    InvalidWorkerArgv,
    NonUtf8Payload,
    InvalidHeader,
    Truncated,
    NonUtf8Text,
    TrailingBytes,
    OddHexLength,
    NonHexData,
    RetiredFieldTag(u8),
    UnknownFieldTag(u8),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidWorkerArgv => {
                f.write_str("worker argv must be exactly --worker-request-v4 PAYLOAD")
            }
            Self::NonUtf8Payload => f.write_str("worker request payload is not UTF-8"),
            Self::InvalidHeader => f.write_str("invalid worker request header"),
            Self::Truncated => f.write_str("truncated worker request"),
            Self::NonUtf8Text => f.write_str("worker request text is not UTF-8"),
            Self::TrailingBytes => f.write_str("trailing worker request bytes"),
            Self::OddHexLength => f.write_str("worker request payload has odd length"),
            Self::NonHexData => f.write_str("worker request payload contains non-hexadecimal data"),
            Self::RetiredFieldTag(tag) => write!(f, "retired field tag {tag}"),
            Self::UnknownFieldTag(tag) => write!(f, "unknown field tag {tag}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

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
        Field::DumpCore => out.push(5),
        Field::AllClosed => out.push(6),
        Field::TargetModuleOnly => out.push(7),
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
        Field::BindGen(value) => {
            out.push(11);
            out.extend_from_slice(&value.to_le_bytes());
        }
        Field::HarnessProfile => out.push(24),
        Field::InspectType(value) => tagged_frame(out, 26, OsStr::new(value)),
        Field::InspectInfo(value) => tagged_frame(out, 27, OsStr::new(value)),
        Field::InspectOut(value) => tagged_frame(out, 28, value),
    }
}

fn tagged_frame(out: &mut Vec<u8>, tag: u8, value: &OsStr) {
    out.push(tag);
    push_frame(out, value);
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn unhex(text: &str) -> Result<Vec<u8>, ProtocolError> {
    if !text.len().is_multiple_of(2) {
        return Err(ProtocolError::OddHexLength);
    }
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_digit(pair[0])?;
            let low = hex_digit(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_digit(byte: u8) -> Result<u8, ProtocolError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(ProtocolError::NonHexData),
    }
}

fn value<'a>(
    args: &mut impl Iterator<Item = &'a OsString>,
    option: &str,
) -> Result<&'a OsStr, CliError> {
    args.next()
        .map(OsString::as_os_str)
        .ok_or_else(|| CliError::new(format!("{option} requires a value")))
}

fn text_value<'a>(
    args: &mut impl Iterator<Item = &'a OsString>,
    option: &str,
) -> Result<&'a str, CliError> {
    value(args, option)?
        .to_str()
        .ok_or_else(|| CliError::new(format!("{option} requires UTF-8 text")))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliError {
    message: String,
}

impl CliError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

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

    #[test]
    fn worker_argv_contains_only_the_typed_protocol() {
        let request =
            ExtractRequest::from_cli(&["Expr.hs".into(), "--target".into(), "answer".into()])
                .unwrap();
        let argv = request.worker_argv();
        assert_eq!(argv.len(), 2);
        assert_eq!(argv[0], WORKER_REQUEST_FLAG);
        let decoded = ExtractRequest::decode_worker_argv(&argv).unwrap();
        assert_eq!(decoded.target_names(), ["answer"]);
    }

    #[test]
    fn worker_argv_rejects_retired_version_markers() {
        let request = ExtractRequest::from_cli(&["Expr.hs".into()]).unwrap();
        let payload = OsString::from(hex(&request.encode()));
        for flag in ["--worker-request-v1", "--worker-request-v2"] {
            assert_eq!(
                ExtractRequest::decode_worker_argv(&[flag.into(), payload.clone()]).unwrap_err(),
                ProtocolError::InvalidWorkerArgv
            );
        }
    }

    #[test]
    fn typed_protocol_rejects_retired_magic_versions() {
        for magic in [b"TPREQ001", b"TPREQ002"] {
            let mut request = magic.to_vec();
            request.extend_from_slice(&0u32.to_le_bytes());
            assert_eq!(
                ExtractRequest::decode(&request).unwrap_err(),
                ProtocolError::InvalidHeader
            );
        }
    }

    #[test]
    fn typed_protocol_round_trips_every_field_shape() {
        let args = vec![
            "Expr.hs".into(),
            "--output-dir".into(),
            "/tmp/out".into(),
            "--targets".into(),
            "a,b".into(),
            "--include".into(),
            "/tmp/include".into(),
            "--turn".into(),
            "--turn-template".into(),
            "expr=/tmp/template.hs".into(),
            "--turn-out".into(),
            "/tmp/turn.cbor".into(),
            "--turn-verdict".into(),
            "expr".into(),
            "--build-products-dir".into(),
            "/tmp/build".into(),
            "--session-root".into(),
            "/tmp/session".into(),
            "--inject-val".into(),
            "M".into(),
            "--bind-gen".into(),
            "7".into(),
            "--harness-profile".into(),
        ];
        let request = ExtractRequest::from_cli(&args).unwrap();
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert_eq!(decoded.cli_argv(), request.cli_argv());
    }

    #[test]
    fn typed_protocol_rejects_unknown_and_truncated_fields() {
        let mut unknown = MAGIC.to_vec();
        unknown.extend_from_slice(&1u32.to_le_bytes());
        unknown.push(255);
        assert_eq!(
            ExtractRequest::decode(&unknown).unwrap_err(),
            ProtocolError::UnknownFieldTag(255)
        );

        let request = ExtractRequest::from_cli(&["Expr.hs".into()]).unwrap();
        let mut truncated = request.encode();
        truncated.pop();
        assert_eq!(
            ExtractRequest::decode(&truncated).unwrap_err(),
            ProtocolError::Truncated
        );
    }

    #[test]
    fn typed_protocol_rejects_retired_session_bind_fields_explicitly() {
        for tag in [9, 10, 14] {
            let mut request = MAGIC.to_vec();
            request.extend_from_slice(&1u32.to_le_bytes());
            request.push(tag);
            assert_eq!(
                ExtractRequest::decode(&request).unwrap_err(),
                ProtocolError::RetiredFieldTag(tag)
            );
        }
    }

    #[test]
    fn cli_rejects_unknown_options() {
        let error = ExtractRequest::from_cli(&["--traget".into(), "answer".into()]).unwrap_err();
        assert_eq!(error.to_string(), "unknown option: --traget");
    }

    #[test]
    fn cli_rejects_invalid_typed_values() {
        let error = ExtractRequest::from_cli(&["--bind-gen".into(), "nope".into()]).unwrap_err();
        assert_eq!(error.to_string(), "--bind-gen requires an unsigned integer");
    }

    #[test]
    fn cli_requires_an_input() {
        let error = ExtractRequest::from_cli(&["--target".into(), "answer".into()]).unwrap_err();
        assert_eq!(error.to_string(), "an input file is required");
    }
}
