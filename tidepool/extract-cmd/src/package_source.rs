//! The existing owned daemon's retained package source and authenticated launcher.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use sha2::{Digest, Sha256};

pub(crate) const CONFIG_ENV: &str = "TIDEPOOL_COMPILER_SERVICE_SOURCE_CONFIG";
pub(crate) const CONFIG_SHA_ENV: &str = "TIDEPOOL_COMPILER_SERVICE_SOURCE_CONFIG_SHA256";
pub(crate) const BOOT_ENV: [&str; 6] = [
    "TIDEPOOL_COMPILER_PACKAGE_PROFILE",
    "TIDEPOOL_COMPILER_PACKAGE_PROFILE_SHA256",
    "TIDEPOOL_COMPILER_SERVICE_EPOCH",
    "TIDEPOOL_COMPILER_PACKAGE_SOURCE_ID",
    "TIDEPOOL_COMPILER_PACKAGE_PROFILE_ID",
    "TIDEPOOL_COMPILER_WORKER_INSTANCE",
];
const LIMIT: usize = 16 * 1024 * 1024;
const HANDOFF: &str = "compiler-service-source.receipt.bin";

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

pub(crate) fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(crate) fn parse_hex(value: &str) -> io::Result<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("expected lowercase SHA256 identity"));
    }
    let mut result = [0; 32];
    for (slot, pair) in result.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        *slot =
            u8::from_str_radix(std::str::from_utf8(pair).map_err(invalid)?, 16).map_err(invalid)?;
    }
    Ok(result)
}

fn read_bounded(file: &File) -> io::Result<Vec<u8>> {
    if file.metadata()?.len() > LIMIT as u64 {
        return Err(invalid("package source control file exceeds 16 MiB"));
    }
    let mut bytes = Vec::new();
    file.take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > LIMIT {
        return Err(invalid("package source control file grew beyond budget"));
    }
    Ok(bytes)
}

struct Decoder<'a>(&'a [u8]);
impl<'a> Decoder<'a> {
    fn take(&mut self, count: usize) -> io::Result<&'a [u8]> {
        let (head, tail) = self
            .0
            .split_at_checked(count)
            .ok_or_else(|| invalid("truncated package source control"))?;
        self.0 = tail;
        Ok(head)
    }
    fn hash(&mut self) -> io::Result<[u8; 32]> {
        self.take(32)?.try_into().map_err(invalid)
    }
    fn frame(&mut self) -> io::Result<&'a [u8]> {
        let count = u32::from_le_bytes(self.take(4)?.try_into().map_err(invalid)?) as usize;
        self.take(count)
    }
    fn path(&mut self) -> io::Result<PathBuf> {
        let text = std::str::from_utf8(self.frame()?).map_err(invalid)?;
        let path = PathBuf::from(text);
        if text.contains('\0')
            || !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
        {
            return Err(invalid(
                "package source path must be absolute and normalized",
            ));
        }
        Ok(path)
    }
    fn end(self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(invalid("trailing package source control bytes"))
        }
    }
}

fn capture_provider(file: &File, expected: &[u8; 32]) -> io::Result<File> {
    let bytes = read_bounded(file)?;
    if &digest(&bytes) != expected {
        return Err(invalid("declared service provider bytes differ"));
    }
    let name = CString::new("tidepool-service-provider").expect("literal");
    // SAFETY: literal C string and Linux memfd flags; ownership transfers once.
    let fd =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut provider = unsafe { File::from_raw_fd(fd) };
    provider.write_all(&bytes)?;
    provider.rewind()?;
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, seals) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(provider)
}

/// The independently configured digest is checked before a provider can run.
/// Admission later compares this same locally measured digest with the declared
/// deployment. Merely choosing environment data cannot issue that admission.
pub(crate) struct ConfiguredSource {
    python: File,
    provider: File,
    record: PathBuf,
    record_sha: [u8; 32],
    source: [u8; 32],
    profile: [u8; 32],
    launch_sha: [u8; 32],
}

impl ConfiguredSource {
    pub(crate) fn from_environment() -> io::Result<Option<Arc<Self>>> {
        let path = std::env::var_os(CONFIG_ENV);
        let seal = std::env::var(CONFIG_SHA_ENV);
        match (path, seal) {
            (None, Err(std::env::VarError::NotPresent)) => Ok(None),
            (Some(path), Ok(seal)) => {
                let file = File::open(path)?;
                let bytes = read_bounded(&file)?;
                let expected = parse_hex(&seal)?;
                if digest(&bytes) != expected {
                    return Err(invalid(
                        "compiler service launch configuration digest differs",
                    ));
                }
                Self::decode(&bytes, expected).map(|config| Some(Arc::new(config)))
            }
            _ => Err(invalid(
                "compiler service configuration requires path and independent digest",
            )),
        }
    }

    fn decode(bytes: &[u8], launch_sha: [u8; 32]) -> io::Result<Self> {
        let mut decoder = Decoder(bytes);
        if decoder.take(8)? != b"TPSCF002" {
            return Err(invalid("unsupported package source configuration version"));
        }
        if !matches!(decoder.take(1)?, [0] | [1]) {
            return Err(invalid("unknown package source configuration kind"));
        }
        let python_path = decoder.path()?;
        let python_sha = decoder.hash()?;
        let provider_path = decoder.path()?;
        let provider_sha = decoder.hash()?;
        let record = decoder.path()?;
        let record_sha = decoder.hash()?;
        let source = decoder.hash()?;
        let profile = decoder.hash()?;
        decoder.end()?;
        let canonical = std::fs::canonicalize(&python_path)?;
        if canonical != python_path || !canonical.starts_with("/nix/store") {
            return Err(invalid(
                "service verifier Python must be its immutable declared Nix executable",
            ));
        }
        let mut python = File::open(canonical)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = python.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        if <[u8; 32]>::from(hasher.finalize()) != python_sha {
            return Err(invalid("declared Python executable bytes differ"));
        }
        python.rewind()?;
        let provider = capture_provider(&File::open(provider_path)?, &provider_sha)?;
        Ok(Self {
            python,
            provider,
            record,
            record_sha,
            source,
            profile,
            launch_sha,
        })
    }

    fn command(&self) -> Command {
        let mut command =
            crate::process::command(format!("/proc/self/fd/{}", self.python.as_raw_fd()));
        let script_fd = self.provider.as_raw_fd();
        command.arg("-I").arg(format!("/proc/self/fd/{script_fd}"));
        command
            .env_clear()
            .env("PATH", "/run/current-system/sw/bin")
            .env("LC_ALL", "C.UTF-8");
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(script_fd, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command
    }

    pub(crate) fn acquire(
        self: &Arc<Self>,
        epoch: [u8; 32],
        owner: crate::frontend::OwnedServiceStartup,
    ) -> io::Result<Arc<ServicePackageSource>> {
        for key in ["LD_PRELOAD", "LD_AUDIT", "LD_LIBRARY_PATH"] {
            if std::env::var_os(key).is_some_and(|value| !value.is_empty()) {
                return Err(invalid(format!(
                    "rooted Nix-only service refuses ambient {key}"
                )));
            }
        }
        let output = self
            .command()
            .arg("acquire-compiler-service-source")
            .arg("--record")
            .arg(&self.record)
            .arg("--record-sha256")
            .arg(crate::endpoint::hex(&self.record_sha))
            .arg("--source-id")
            .arg(crate::endpoint::hex(&self.source))
            .arg("--profile-id")
            .arg(crate::endpoint::hex(&self.profile))
            .arg("--service-epoch")
            .arg(crate::endpoint::hex(&epoch))
            .arg("--owner-directory")
            .arg(owner.directory())
            .output()?;
        if !output.status.success() {
            return Err(invalid(format!(
                "compiler source acquisition failed: {}; retained owner {}",
                output.status,
                owner.directory().display()
            )));
        }
        let mut source = self.decode_receipt(&output.stdout, epoch)?;
        source._owner = Some(Arc::new(owner));
        tidepool_atomic_write::write_durable(
            &source
                ._owner
                .as_ref()
                .expect("actual owner")
                .directory()
                .join(HANDOFF),
            &output.stdout,
        )
        .map_err(io::Error::other)?;
        Ok(Arc::new(source))
    }

    fn decode_receipt(
        self: &Arc<Self>,
        bytes: &[u8],
        epoch: [u8; 32],
    ) -> io::Result<ServicePackageSource> {
        let ReceiptMetadata {
            receipt,
            receipt_sha,
            profile_path,
            profile_sha,
        } = self.decode_receipt_metadata(bytes, epoch)?;
        if std::fs::canonicalize(&profile_path)? != profile_path
            || !profile_path.starts_with("/nix/store")
        {
            return Err(invalid("service profile must be an immutable Nix object"));
        }
        let profile_bytes = read_bounded(&File::open(&profile_path)?)?;
        if digest(&profile_bytes) != profile_sha {
            return Err(invalid("immutable service profile digest differs"));
        }
        let mut profile_decoder = Decoder(&profile_bytes);
        if profile_decoder.take(8)? != b"TPSPF001" {
            return Err(invalid("unsupported immutable service profile version"));
        }
        let facts = profile_decoder.frame()?;
        if !facts.starts_with(b"TPGHC002") {
            return Err(invalid("unsupported GHC profile facts version"));
        }
        let mut profile_identity = Sha256::new();
        profile_identity.update(b"Tidepool GHC supported package profile v2\0");
        profile_identity.update(facts);
        if <[u8; 32]>::from(profile_identity.finalize()) != self.profile {
            return Err(invalid("immutable profile identity differs"));
        }
        Ok(ServicePackageSource {
            configuration: Arc::clone(self),
            epoch,
            receipt,
            receipt_sha,
            profile_path,
            profile_sha,
            commitment: digest(facts),
            _owner: None,
        })
    }

    fn decode_receipt_metadata(
        &self,
        bytes: &[u8],
        epoch: [u8; 32],
    ) -> io::Result<ReceiptMetadata> {
        let mut decoder = Decoder(bytes);
        if decoder.take(8)? != b"TPSRC001"
            || decoder.hash()? != epoch
            || decoder.hash()? != self.source
            || decoder.hash()? != self.profile
        {
            return Err(invalid(
                "service acquisition receipt differs from configured epoch/source/profile",
            ));
        }
        let receipt = decoder.path()?;
        let receipt_sha = decoder.hash()?;
        let profile_path = decoder.path()?;
        let profile_sha = decoder.hash()?;
        decoder.end()?;
        if digest(&read_bounded(&File::open(&receipt)?)?) != receipt_sha {
            return Err(invalid("service receipt bytes differ"));
        }
        Ok(ReceiptMetadata {
            receipt,
            receipt_sha,
            profile_path,
            profile_sha,
        })
    }

    pub(crate) fn release_after_complete(
        self: &Arc<Self>,
        owner: &Path,
        epoch: [u8; 32],
    ) -> io::Result<()> {
        let bytes = read_bounded(&File::open(owner.join(HANDOFF))?)?;
        let source = self.decode_receipt_metadata(&bytes, epoch)?;
        let completion = source
            .receipt
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| invalid("invalid receipt owner"))?
            .join("compiler-service-complete-retirement.json");
        let json = format!("{{\"schema\":1,\"kind\":\"compiler-service-complete-retirement\",\"service_epoch\":\"{}\",\"source_id\":\"{}\",\"profile_id\":\"{}\",\"receipt_sha256\":\"{}\",\"complete\":true}}\n",crate::endpoint::hex(&epoch),crate::endpoint::hex(&self.source),crate::endpoint::hex(&self.profile),crate::endpoint::hex(&source.receipt_sha));
        tidepool_atomic_write::write_durable(&completion, json.as_bytes())
            .map_err(io::Error::other)?;
        let status = self
            .command()
            .arg("release-compiler-service-source")
            .arg("--receipt")
            .arg(&source.receipt)
            .arg("--receipt-sha256")
            .arg(crate::endpoint::hex(&source.receipt_sha))
            .arg("--complete-retirement")
            .arg(completion)
            .stdout(Stdio::inherit())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(invalid(format!(
                "service package source release failed: {status}"
            )))
        }
    }
}

struct ReceiptMetadata {
    receipt: PathBuf,
    receipt_sha: [u8; 32],
    profile_path: PathBuf,
    profile_sha: [u8; 32],
}

/// Strongly retained by every live or quarantined worker. Drop has no release
/// side effect; only the outer owned daemon can prove complete retirement.
pub(crate) struct ServicePackageSource {
    configuration: Arc<ConfiguredSource>,
    epoch: [u8; 32],
    receipt: PathBuf,
    receipt_sha: [u8; 32],
    profile_path: PathBuf,
    profile_sha: [u8; 32],
    commitment: [u8; 32],
    _owner: Option<Arc<crate::frontend::OwnedServiceStartup>>,
}

impl ServicePackageSource {
    pub(crate) fn configure_worker(
        &self,
        command: &mut Command,
        instance: &[u8; 32],
    ) -> io::Result<()> {
        for key in ["LD_PRELOAD", "LD_AUDIT", "LD_LIBRARY_PATH"] {
            command.env_remove(key);
        }
        let values = [
            self.profile_path.as_os_str().to_owned(),
            crate::endpoint::hex(&self.profile_sha).into(),
            crate::endpoint::hex(&self.epoch).into(),
            crate::endpoint::hex(&self.configuration.source).into(),
            crate::endpoint::hex(&self.configuration.profile).into(),
            crate::endpoint::hex(instance).into(),
        ];
        command.envs(BOOT_ENV.into_iter().zip(values));
        Ok(())
    }
    pub(crate) fn check_boot(
        self: &Arc<Self>,
        bytes: &[u8],
        instance: &[u8; 32],
    ) -> io::Result<WorkerPackageBootReceipt> {
        let mut decoder = Decoder(bytes);
        if decoder.take(8)? != b"TPBOOT01"
            || decoder.hash()? != *instance
            || decoder.hash()? != self.epoch
            || decoder.hash()? != self.configuration.source
            || decoder.hash()? != self.configuration.profile
            || decoder.hash()? != self.commitment
        {
            return Err(invalid(
                "actual GHC worker boot differs from retained service profile",
            ));
        }
        decoder.end()?;
        Ok(WorkerPackageBootReceipt {
            source: Arc::clone(self),
        })
    }

    pub(crate) fn association(
        self: &Arc<Self>,
        receipts: &[Option<WorkerPackageBootReceipt>],
    ) -> io::Result<crate::CompilerPackageSourceAssociation> {
        crate::endpoint::CompilerPackageSourceAssociation::from_booted_source(self, receipts)
    }

    pub(crate) fn validated_association_parts(
        &self,
        receipts: &[Option<WorkerPackageBootReceipt>],
    ) -> io::Result<[[u8; 32]; 5]> {
        if receipts.is_empty()
            || receipts.iter().any(|receipt| {
                !receipt
                    .as_ref()
                    .is_some_and(|receipt| std::ptr::eq(self, receipt.source.as_ref()))
            })
        {
            return Err(invalid(
                "service readiness requires actual boot of every configured worker",
            ));
        }
        Ok([
            self.configuration.source,
            self.configuration.profile,
            self.epoch,
            self.commitment,
            self.configuration.launch_sha,
        ])
    }
}

#[derive(Clone)]
pub(crate) struct WorkerPackageBootReceipt {
    source: Arc<ServicePackageSource>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticated_provider_retains_sealed_captured_bytes() {
        let mut original = tempfile::tempfile().unwrap();
        original.write_all(b"print('captured')\n").unwrap();
        original.rewind().unwrap();
        let mut captured = capture_provider(&original, &digest(b"print('captured')\n")).unwrap();
        original.set_len(0).unwrap();
        original.write_all(b"print('replacement')\n").unwrap();
        let mut bytes = Vec::new();
        captured.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"print('captured')\n");
        assert!(captured.write_all(b"altered").is_err());
        let all = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
        assert_eq!(
            unsafe { libc::fcntl(captured.as_raw_fd(), libc::F_GET_SEALS) } & all,
            all
        );
        original.rewind().unwrap();
        assert!(capture_provider(&original, &digest(b"print('captured')\n")).is_err());
    }

    fn source() -> Arc<ServicePackageSource> {
        Arc::new(ServicePackageSource {
            configuration: Arc::new(ConfiguredSource {
                python: File::open("/dev/null").unwrap(),
                provider: File::open("/dev/null").unwrap(),
                record: "/unused".into(),
                record_sha: [1; 32],
                source: [2; 32],
                profile: [3; 32],
                launch_sha: [4; 32],
            }),
            epoch: [5; 32],
            receipt: "/unused/receipt".into(),
            receipt_sha: [6; 32],
            profile_path: "/unused/profile".into(),
            profile_sha: [7; 32],
            commitment: [8; 32],
            _owner: None,
        })
    }

    #[test]
    fn association_requires_same_service_actual_boot_and_retains_its_source() {
        let source = source();
        let weak = Arc::downgrade(&source);
        let instance = [9; 32];
        let mut bytes = b"TPBOOT01".to_vec();
        for part in [instance, [5; 32], [2; 32], [3; 32], [8; 32]] {
            bytes.extend_from_slice(&part);
        }
        let receipt = source.check_boot(&bytes, &instance).unwrap();
        assert!(source.check_boot(&bytes, &[10; 32]).is_err());
        assert!(source.association(&[]).is_err());
        assert!(source.association(&[None]).is_err());
        assert!(self::source()
            .association(&[Some(receipt.clone())])
            .is_err());
        let association = source.association(&[Some(receipt.clone())]).unwrap();
        assert_eq!(association.service_epoch(), &[5; 32]);
        assert_eq!(association.launch_configuration_sha256(), &[4; 32]);
        let mut wire = Vec::new();
        crate::CompilerPackageSourceAssociation::write_optional(Some(&association), &mut wire)
            .unwrap();
        assert_eq!(
            crate::CompilerPackageSourceAssociation::read_optional(&wire[..], &[5; 32]).unwrap(),
            Some(association)
        );
        assert!(
            crate::CompilerPackageSourceAssociation::read_optional(&wire[..], &[0; 32]).is_err()
        );
        drop(source);
        assert!(weak.upgrade().is_some());
        drop(receipt);
        assert!(weak.upgrade().is_none());
    }
}
