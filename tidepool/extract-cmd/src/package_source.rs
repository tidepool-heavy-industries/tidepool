//! The existing owned daemon's retained package source and authenticated launcher.

use crate::service::{AcquisitionObservation, CompleteServiceRetirement, ServiceReference};
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
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
const ATTEMPT: &str = "compiler-service-source.attempt.json";
const PROVIDER_COMPLETE: &str = "compiler-service-source.provider-complete.bin";
const ACQUIRED: &str = "compiler-service-source.acquired.bin";
const HANDOFF: &str = "compiler-service-source.receipt.bin";
const FINALIZATION: &str = "compiler-service-source.finalization.bin";
const FINALIZED: &str = "compiler-service-source.finalized.bin";
const COMPLETION: &str = "compiler-service-complete-retirement.json";

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
    python_object: PathBuf,
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
        let python_object = nix_object(&canonical)?;
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
            python_object,
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

    fn attempt(&self, epoch: [u8; 32], owner: &Path) -> io::Result<ServiceReference> {
        if std::fs::canonicalize(owner)? != owner {
            return Err(invalid("acquisition owner is not canonical"));
        }
        let record_bytes = read_bounded(&File::open(&self.record)?)?;
        if digest(&record_bytes) != self.record_sha {
            return Err(invalid("acquisition source record changed"));
        }
        let record: serde_json::Value = serde_json::from_slice(&record_bytes)?;
        let text = |key: &str| {
            record[key]
                .as_str()
                .ok_or_else(|| invalid(format!("missing source {key}")))
        };
        if record["schema"] != 2 || record["kind"] != "compiler-service-package-source" {
            return Err(invalid("unsupported declared acquisition scope version"));
        }
        if text("source_id")? != crate::endpoint::hex(&self.source)
            || text("profile_id")? != crate::endpoint::hex(&self.profile)
        {
            return Err(invalid("acquisition record association differs"));
        }
        if Path::new(text("provider_guard_store_path")?) != self.python_object {
            return Err(invalid(
                "declared provider guard differs from authenticated Python",
            ));
        }
        let objects = record["acquisition_roots"]
            .as_array()
            .ok_or_else(|| invalid("missing declared exact acquisition scope"))?;
        if objects.is_empty() {
            return Err(invalid("empty declared acquisition scope"));
        }
        let mut seen = std::collections::BTreeSet::new();
        let roots = objects.iter().map(|value| {
            let value=value.as_str().ok_or_else(||invalid("invalid declared acquisition root"))?;
            if !seen.insert(value) {return Err(invalid("duplicate declared acquisition root"));}
            let path = Path::new(&value);
            if nix_object(path)? != path { return Err(invalid("acquisition root is not a top-level Nix object")); }
            Ok(serde_json::json!({"path": owner.join("package-source/gc-roots").join(crate::endpoint::hex(&digest(value.as_bytes()))), "store_path":value}))
        }).collect::<io::Result<Vec<_>>>()?;
        let mut nonce = [0; 32];
        File::open("/dev/urandom")?.read_exact(&mut nonce)?;
        let attempt = serde_json::json!({"schema":1,"kind":"compiler-service-package-acquisition-attempt",
            "attempt_id":crate::endpoint::hex(&nonce),"owner_directory":owner,
            "service_epoch":crate::endpoint::hex(&epoch),"source_id":crate::endpoint::hex(&self.source),
            "profile_id":crate::endpoint::hex(&self.profile),"source_record":self.record,
            "source_record_sha256":crate::endpoint::hex(&self.record_sha),
            "launch_configuration_sha256":crate::endpoint::hex(&self.launch_sha),
            "profile_store_path":text("profile_store_path")?,"profile_sha256":text("profile_sha256")?,"roots":roots});
        let bytes = serde_json::to_vec(&attempt)?;
        let path = owner.join(ATTEMPT);
        // This fresh owner's attempt is never replaced by provider progress.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        File::open(owner)?.sync_all()?;
        Ok(ServiceReference {
            path,
            sha256: digest(&bytes),
        })
    }

    pub(crate) fn acquire(
        self: &Arc<Self>,
        epoch: [u8; 32],
        owner: Arc<crate::frontend::OwnedServiceStartup>,
    ) -> Result<Arc<ServicePackageSource>, AcquisitionFailure> {
        let mut observation = AcquisitionObservation::Unknown {
            attempt: None,
            failure: "provider not started".into(),
        };
        let result = (|| -> io::Result<Arc<ServicePackageSource>> {
            let attempt = self.attempt(epoch, owner.directory())?;
            observation = AcquisitionObservation::Unknown {
                attempt: Some(attempt.clone()),
                failure: "provider outcome not validated".into(),
            };
            for key in ["LD_PRELOAD", "LD_AUDIT", "LD_LIBRARY_PATH"] {
                if std::env::var_os(key).is_some_and(|value| !value.is_empty()) {
                    return Err(invalid(format!(
                        "rooted Nix-only service refuses ambient {key}"
                    )));
                }
            }
            // File-backed capture bounds native materialization even for a broken provider.
            let stdout_path = owner.directory().join("package-provider.stdout");
            let stderr_path = owner.directory().join("package-provider.stderr");
            let capture = |path: &Path| {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)
            };
            let mut command = self.command();
            command
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
                .arg("--attempt")
                .arg(&attempt.path)
                .arg("--attempt-sha256")
                .arg(crate::endpoint::hex(&attempt.sha256))
                .stdout(capture(&stdout_path)?)
                .stderr(capture(&stderr_path)?);
            let status = provider_status(&mut command, owner.deadline, true)?;
            let bytes = read_bounded(&File::open(&stdout_path)?)?;
            let parsed = decode_provider_frame(&bytes)?;
            self.validate_outcome(&parsed, &attempt, epoch, owner.directory())?;
            if !status.success() {
                return Err(invalid(format!(
                    "compiler source provider exit/frame mismatch: {status}; stderr retained at {}",
                    stderr_path.display()
                )));
            }
            observation = parsed;
            let mut completion = b"TPSPC001".to_vec();
            for hash in [epoch, attempt.sha256, digest(&bytes)] {
                completion.extend_from_slice(&hash);
            }
            if let Err(publication_error) = tidepool_atomic_write::write_durable(
                &owner.directory().join(PROVIDER_COMPLETE),
                &completion,
            ) {
                if let AcquisitionObservation::Refused {
                    primary,
                    publication,
                    ..
                } = &mut observation
                {
                    publication.push(publication_error.to_string());
                    return Err(invalid(primary.clone()));
                }
                return Err(io::Error::other(publication_error));
            }
            let AcquisitionObservation::Acquired {
                receipt, profile, ..
            } = &observation
            else {
                return Err(invalid(match &observation {
                    AcquisitionObservation::Refused { primary, .. } => primary.clone(),
                    _ => "provider did not acquire source".into(),
                }));
            };
            let carrier = self.receipt_carrier(epoch, receipt, profile)?;
            // Retain validated receipt metadata before opening the profile or publishing HANDOFF.
            tidepool_atomic_write::write_durable(&owner.directory().join(ACQUIRED), &carrier)
                .map_err(io::Error::other)?;
            if let AcquisitionObservation::Acquired { publication, .. } = &observation {
                if !publication.is_empty() {
                    return Err(invalid(format!(
                        "acquired receipt publication incomplete: {}",
                        publication.join("; ")
                    )));
                }
            }
            let mut source = self.decode_receipt(&carrier, epoch)?;
            source.acquisition = observation.clone();
            source._owner = Some(Arc::clone(&owner));
            tidepool_atomic_write::write_durable(&owner.directory().join(HANDOFF), &carrier)
                .map_err(io::Error::other)?;
            Ok(Arc::new(source))
        })();
        result.map_err(|error| {
            if let AcquisitionObservation::Unknown { failure, .. } = &mut observation {
                *failure = error.to_string();
            }
            AcquisitionFailure { observation, error }
        })
    }

    fn validate_outcome(
        &self,
        outcome: &AcquisitionObservation,
        attempt: &ServiceReference,
        epoch: [u8; 32],
        owner: &Path,
    ) -> io::Result<()> {
        if attempt.path != owner.join(ATTEMPT) {
            return Err(invalid("source attempt belongs to another owner"));
        }
        let attempt_bytes = read_bounded(&File::open(&attempt.path)?)?;
        if digest(&attempt_bytes) != attempt.sha256 {
            return Err(invalid("source attempt digest differs"));
        }
        let plan: serde_json::Value = serde_json::from_slice(&attempt_bytes)?;
        if plan["schema"] != 1
            || plan["kind"] != "compiler-service-package-acquisition-attempt"
            || plan["owner_directory"] != owner.to_string_lossy().as_ref()
            || plan["service_epoch"] != crate::endpoint::hex(&epoch)
            || plan["source_id"] != crate::endpoint::hex(&self.source)
            || plan["profile_id"] != crate::endpoint::hex(&self.profile)
            || plan["launch_configuration_sha256"] != crate::endpoint::hex(&self.launch_sha)
            || plan["source_record_sha256"] != crate::endpoint::hex(&self.record_sha)
        {
            return Err(invalid("source attempt association differs"));
        }
        match outcome {
            AcquisitionObservation::Acquired {
                attempt: actual,
                receipt,
                profile,
                ..
            } => {
                if actual != attempt || receipt.path != owner.join("package-source/receipt.json") {
                    return Err(invalid("provider acquired another attempt"));
                }
                let bytes = read_bounded(&File::open(&receipt.path)?)?;
                if digest(&bytes) != receipt.sha256 {
                    return Err(invalid("provider receipt digest differs"));
                }
                let state: serde_json::Value = serde_json::from_slice(&bytes)?;
                if state["schema"] != 2
                    || state["kind"] != "compiler-service-package-acquisition"
                    || state["status"] != "acquired"
                    || state["service_epoch"] != crate::endpoint::hex(&epoch)
                    || state["source_id"] != crate::endpoint::hex(&self.source)
                    || state["profile_id"] != crate::endpoint::hex(&self.profile)
                    || state["attempt_sha256"] != crate::endpoint::hex(&attempt.sha256)
                    || state["source_record_sha256"] != crate::endpoint::hex(&self.record_sha)
                    || state["provider_guard_store_path"]
                        != self.python_object.to_string_lossy().as_ref()
                    || state["acquire_count"] != 1
                    || state["release_count"] != 0
                    || profile.path
                        != PathBuf::from(
                            plan["profile_store_path"]
                                .as_str()
                                .ok_or_else(|| invalid("missing attempt profile"))?,
                        )
                    || crate::endpoint::hex(&profile.sha256) != plan["profile_sha256"]
                {
                    return Err(invalid("provider receipt association differs"));
                }
                let roots = state["roots"]
                    .as_array()
                    .ok_or_else(|| invalid("missing receipt roots"))?;
                if roots.iter().any(|item| item["state"] != "registered") {
                    return Err(invalid(
                        "provider acquired receipt contains unregistered roots",
                    ));
                }
                let exact=roots.iter().map(|item|serde_json::json!({"path":item["path"],"store_path":item["store_path"]})).collect::<Vec<_>>();
                if serde_json::Value::Array(exact) != plan["roots"] {
                    return Err(invalid("provider receipt scope differs"));
                }
                Ok(())
            }
            AcquisitionObservation::Refused {
                attempt: actual, ..
            } if actual == attempt => Ok(()),
            _ => Err(invalid("invalid provider acquisition outcome")),
        }
    }

    fn receipt_carrier(
        &self,
        epoch: [u8; 32],
        receipt: &ServiceReference,
        profile: &ServiceReference,
    ) -> io::Result<Vec<u8>> {
        let mut bytes = b"TPSRC001".to_vec();
        for hash in [epoch, self.source, self.profile] {
            bytes.extend_from_slice(&hash);
        }
        encode_path(&mut bytes, &receipt.path)?;
        bytes.extend_from_slice(&receipt.sha256);
        encode_path(&mut bytes, &profile.path)?;
        bytes.extend_from_slice(&profile.sha256);
        Ok(bytes)
    }

    fn retained_carrier(&self, owner: &Path, epoch: [u8; 32]) -> io::Result<Vec<u8>> {
        let attempt_path = owner.join(ATTEMPT);
        let attempt_bytes = read_bounded(&File::open(&attempt_path)?)?;
        let attempt = ServiceReference {
            path: attempt_path,
            sha256: digest(&attempt_bytes),
        };
        let bytes = read_bounded(&File::open(owner.join("package-provider.stdout"))?)?;
        let outcome = decode_provider_frame(&bytes)?;
        self.validate_outcome(&outcome, &attempt, epoch, owner)?;
        let AcquisitionObservation::Acquired {
            receipt, profile, ..
        } = outcome
        else {
            return Err(invalid("retained provider did not issue acquired receipt"));
        };
        self.receipt_carrier(epoch, &receipt, &profile)
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
            acquisition: AcquisitionObservation::NotConfigured,
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
        retirement: &CompleteServiceRetirement,
    ) -> io::Result<()> {
        if retirement.owner() != owner {
            return Err(invalid("source retirement owner differs"));
        }
        let epoch = retirement.epoch();
        // Acquired metadata remains usable after profile validation or HANDOFF publication failed.
        let attempt_path = owner.join(ATTEMPT);
        let attempt_bytes = read_bounded(&File::open(&attempt_path)?)?;
        let attempt = ServiceReference {
            path: attempt_path,
            sha256: digest(&attempt_bytes),
        };
        let provider_bytes = read_bounded(&File::open(owner.join("package-provider.stdout"))?)?;
        let complete = read_bounded(&File::open(owner.join(PROVIDER_COMPLETE))?)?;
        let mut expected = b"TPSPC001".to_vec();
        for hash in [epoch, attempt.sha256, digest(&provider_bytes)] {
            expected.extend_from_slice(&hash);
        }
        if complete != expected {
            return Err(invalid(
                "provider successful completion differs from retained attempt/outcome",
            ));
        }
        let outcome = decode_provider_frame(&provider_bytes)?;
        self.validate_outcome(&outcome, &attempt, epoch, owner)?;
        match outcome {
            AcquisitionObservation::Refused {
                settlement:
                    crate::service::SourceSettlement::NoRootsIssued
                    | crate::service::SourceSettlement::PrebootReleased,
                ..
            } => return Ok(()),
            AcquisitionObservation::Refused {
                settlement: crate::service::SourceSettlement::RetainedUnknown,
                ..
            } => {
                let json = serde_json::json!({"schema":2,"kind":"compiler-service-attempt-retirement","complete":true,
                    "owner_directory":owner,"service_epoch":crate::endpoint::hex(&epoch),
                    "source_id":crate::endpoint::hex(&self.source),"profile_id":crate::endpoint::hex(&self.profile),
                    "attempt_sha256":crate::endpoint::hex(&attempt.sha256),"provider_outcome_sha256":crate::endpoint::hex(&digest(&provider_bytes))});
                let complete_path = owner.join(COMPLETION);
                tidepool_atomic_write::write_durable(&complete_path, &serde_json::to_vec(&json)?)
                    .map_err(io::Error::other)?;
                let mut command = self.command();
                command
                    .arg("settle-compiler-service-attempt")
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
                    .arg(owner)
                    .arg("--attempt")
                    .arg(&attempt.path)
                    .arg("--attempt-sha256")
                    .arg(crate::endpoint::hex(&attempt.sha256))
                    .arg("--provider-outcome")
                    .arg(owner.join("package-provider.stdout"))
                    .arg("--provider-outcome-sha256")
                    .arg(crate::endpoint::hex(&digest(&provider_bytes)))
                    .arg("--complete-retirement")
                    .arg(complete_path)
                    .stdout(Stdio::inherit());
                let status = provider_status(
                    &mut command,
                    std::time::Instant::now() + crate::endpoint::DIRECT_CLOSE_TIMEOUT,
                    false,
                )?;
                return if status.success() {
                    Ok(())
                } else {
                    Err(invalid(format!(
                        "partial source settlement failed: {status}"
                    )))
                };
            }
            AcquisitionObservation::Unknown { .. } | AcquisitionObservation::NotConfigured => {
                return Err(invalid("source acquisition settlement remains unknown"))
            }
            AcquisitionObservation::Acquired { .. } => {}
        }
        let bytes = self.retained_carrier(owner, epoch)?;
        // The exact provider output and attempt suffice even when HANDOFF was
        // never published. This durable native carrier supports final guard recovery.
        tidepool_atomic_write::write_durable(&owner.join(ACQUIRED), &bytes)
            .map_err(io::Error::other)?;
        tidepool_atomic_write::write_durable(&owner.join(HANDOFF), &bytes)
            .map_err(io::Error::other)?;
        let source = self.decode_receipt_metadata(&bytes, epoch)?;
        if source.receipt != owner.join("package-source/receipt.json") {
            return Err(invalid("service receipt is outside the exact owner"));
        }
        let completion = source
            .receipt
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| invalid("invalid receipt owner"))?
            .join(COMPLETION);
        let json = format!("{{\"schema\":1,\"kind\":\"compiler-service-complete-retirement\",\"service_epoch\":\"{}\",\"source_id\":\"{}\",\"profile_id\":\"{}\",\"receipt_sha256\":\"{}\",\"complete\":true}}\n",crate::endpoint::hex(&epoch),crate::endpoint::hex(&self.source),crate::endpoint::hex(&self.profile),crate::endpoint::hex(&source.receipt_sha));
        tidepool_atomic_write::write_durable(&completion, json.as_bytes())
            .map_err(io::Error::other)?;
        let mut command = self.command();
        command
            .arg("release-compiler-service-source")
            .arg("--receipt")
            .arg(&source.receipt)
            .arg("--receipt-sha256")
            .arg(crate::endpoint::hex(&source.receipt_sha))
            .arg("--complete-retirement")
            .arg(&completion)
            .stdout(Stdio::inherit());
        let status = provider_status(
            &mut command,
            std::time::Instant::now() + crate::endpoint::DIRECT_CLOSE_TIMEOUT,
            false,
        )?;
        if !status.success() {
            return Err(invalid(format!(
                "service package source release failed: {status}"
            )));
        }
        // Provider success means the package roots are durably settled while
        // its Python closure remains pinned. Native recovery owns the last root.
        let roots = owner.join("package-source/gc-roots");
        if std::fs::canonicalize(&roots)? != roots {
            return Err(invalid("guard directory is not owned in place"));
        }
        let guard = roots.join(crate::endpoint::hex(&digest(
            self.python_object.as_os_str().as_encoded_bytes(),
        )));
        if std::fs::read_link(&guard)? != self.python_object {
            return Err(invalid(
                "provider did not retain the authenticated Python guard",
            ));
        }
        let mut pending = b"TPSFN001".to_vec();
        for hash in [
            epoch,
            self.source,
            self.profile,
            source.receipt_sha,
            self.launch_sha,
            digest(json.as_bytes()),
            digest(&bytes),
        ] {
            pending.extend_from_slice(&hash);
        }
        let target = self.python_object.as_os_str().as_encoded_bytes();
        let length = u32::try_from(target.len()).map_err(invalid)?;
        pending.extend_from_slice(&length.to_le_bytes());
        pending.extend_from_slice(target);
        pending.extend_from_slice(&digest(&pending));
        tidepool_atomic_write::write_durable(&owner.join(FINALIZATION), &pending)
            .map_err(io::Error::other)?;
        finish_retirement(owner)
    }
}

fn provider_status(
    command: &mut Command,
    deadline: std::time::Instant,
    observe_cancellation: bool,
) -> io::Result<std::process::ExitStatus> {
    if observe_cancellation {
        crate::compiler_host_checkpoint()?;
    }
    if std::time::Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "provider startup deadline expired before spawn",
        ));
    }
    let mut child = crate::process::OwnedChild(command.spawn()?);
    let result = (|| loop {
        if observe_cancellation {
            crate::compiler_host_checkpoint()?;
        }
        if std::time::Instant::now() >= deadline {
            break Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "provider absolute deadline expired",
            ));
        }
        match child.observe()? {
            Some(reaped) => break Ok(reaped.status()),
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    })();
    match result {
        Ok(status) => Ok(status),
        Err(primary) => {
            let (kill, reap) =
                child.terminate(std::time::Instant::now() + crate::endpoint::DIRECT_CLOSE_TIMEOUT);
            let cleanup = match reap {
                crate::CompilerWorkerRetirement::Reaped(_) => "reaped".to_owned(),
                crate::CompilerWorkerRetirement::WaitUnconfirmed(cause) => {
                    format!("reap unconfirmed: {cause:?}")
                }
            };
            Err(io::Error::new(
                primary.kind(),
                format!("{primary}; provider termination: {kill:?}; {cleanup}"),
            ))
        }
    }
}

#[derive(Debug)]
pub(crate) struct AcquisitionFailure {
    observation: AcquisitionObservation,
    pub(crate) error: io::Error,
}
impl AcquisitionFailure {
    pub(crate) fn observation(&self) -> AcquisitionObservation {
        self.observation.clone()
    }
}
impl std::fmt::Display for AcquisitionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for AcquisitionFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
fn encode_path(bytes: &mut Vec<u8>, path: &Path) -> io::Result<()> {
    let value = path.as_os_str().as_encoded_bytes();
    bytes.extend_from_slice(&u32::try_from(value.len()).map_err(invalid)?.to_le_bytes());
    bytes.extend_from_slice(value);
    Ok(())
}
fn decode_provider_frame(bytes: &[u8]) -> io::Result<AcquisitionObservation> {
    let mut decoder = Decoder(bytes);
    if decoder.take(8)? != b"TPSAO001" {
        return Err(invalid("unsupported provider outcome version"));
    }
    let payload = decoder.frame()?;
    if payload.len() > 65536 {
        return Err(invalid("provider outcome exceeds 64 KiB"));
    }
    let result = serde_json::from_slice(payload)?;
    decoder.end()?;
    Ok(result)
}

fn nix_object(path: &Path) -> io::Result<PathBuf> {
    let relative = path.strip_prefix("/nix/store").map_err(invalid)?;
    let first = relative
        .components()
        .next()
        .ok_or_else(|| invalid("missing Nix object"))?;
    let Component::Normal(name) = first else {
        return Err(invalid("invalid Nix object"));
    };
    let text = name
        .to_str()
        .ok_or_else(|| invalid("non-UTF8 Nix object"))?;
    if text.len() < 34
        || text.as_bytes()[32] != b'-'
        || !text[..32]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Err(invalid("invalid Nix object basename"));
    }
    Ok(Path::new("/nix/store").join(name))
}

/// Resume only the native final obligation. No provider, Python executable,
/// environment configuration or released profile is opened on this path.
pub(crate) fn finish_retirement(owner: &Path) -> io::Result<()> {
    if !owner.is_absolute() || std::fs::canonicalize(owner)? != owner {
        return Err(invalid("retirement owner must be its canonical directory"));
    }
    let pending = read_bounded(&File::open(owner.join(FINALIZATION))?)?;
    let (body, seal) = pending
        .split_at_checked(
            pending
                .len()
                .checked_sub(32)
                .ok_or_else(|| invalid("truncated finalization"))?,
        )
        .ok_or_else(|| invalid("truncated finalization"))?;
    if digest(body).as_slice() != seal {
        return Err(invalid("finalization seal differs"));
    }
    let mut decoder = Decoder(body);
    if decoder.take(8)? != b"TPSFN001" {
        return Err(invalid("unsupported finalization version"));
    }
    let epoch = decoder.hash()?;
    let source = decoder.hash()?;
    let profile = decoder.hash()?;
    let receipt_sha = decoder.hash()?;
    let _launch_sha = decoder.hash()?;
    let complete_sha = decoder.hash()?;
    let handoff_sha = decoder.hash()?;
    let target = decoder.path()?;
    decoder.end()?;
    if nix_object(&target)? != target {
        return Err(invalid("guard target is not a top-level Nix object"));
    }
    let handoff = read_bounded(&File::open(owner.join(HANDOFF))?)?;
    if digest(&handoff) != handoff_sha {
        return Err(invalid("finalization handoff differs"));
    }
    let mut receipt = Decoder(&handoff);
    if receipt.take(8)? != b"TPSRC001"
        || receipt.hash()? != epoch
        || receipt.hash()? != source
        || receipt.hash()? != profile
        || receipt.path()? != owner.join("package-source/receipt.json")
        || receipt.hash()? != receipt_sha
    {
        return Err(invalid("finalization receipt association differs"));
    }
    // Decode the unused profile locator as data, without reopening that object.
    receipt.path()?;
    receipt.hash()?;
    receipt.end()?;
    if digest(&read_bounded(&File::open(
        owner.join("package-source/receipt.json"),
    )?)?)
        != receipt_sha
        || digest(&read_bounded(&File::open(owner.join(COMPLETION))?)?) != complete_sha
    {
        return Err(invalid("finalization custody evidence differs"));
    }
    let roots = owner.join("package-source/gc-roots");
    if std::fs::canonicalize(&roots)? != roots {
        return Err(invalid("guard directory is not owned in place"));
    }
    let guard = roots.join(crate::endpoint::hex(&digest(
        target.as_os_str().as_encoded_bytes(),
    )));
    match std::fs::read_link(&guard) {
        Ok(actual) if actual == target => std::fs::remove_file(&guard)?,
        Ok(_) => return Err(invalid("provider guard target differs")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A durable pending record precedes unlink, so absent is a valid
            // interrupted native-finalization state, independent of Nix GC.
            match std::fs::symlink_metadata(&guard) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
                Ok(_) => return Err(invalid("provider guard is not a symlink")),
            }
        }
        Err(error) => return Err(error),
    }
    File::open(&roots)?.sync_all()?;
    tidepool_atomic_write::write_durable(&owner.join(FINALIZED), &digest(&pending))
        .map_err(io::Error::other)?;
    Ok(())
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
    acquisition: AcquisitionObservation,
    _owner: Option<Arc<crate::frontend::OwnedServiceStartup>>,
}

impl ServicePackageSource {
    pub(crate) fn acquisition_observation(&self) -> AcquisitionObservation {
        self.acquisition.clone()
    }
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

    fn pending_retirement() -> (tempfile::TempDir, PathBuf) {
        let owner = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(owner.path()).unwrap();
        std::fs::create_dir_all(root.join("package-source/gc-roots")).unwrap();
        let target = PathBuf::from("/nix/store/00000000000000000000000000000000-python");
        let receipt_bytes = b"exact issued acquisition";
        let completion_bytes = b"complete actual process retirement";
        std::fs::write(root.join("package-source/receipt.json"), receipt_bytes).unwrap();
        std::fs::write(root.join(COMPLETION), completion_bytes).unwrap();
        let mut handoff = b"TPSRC001".to_vec();
        for part in [[1; 32], [2; 32], [3; 32]] {
            handoff.extend_from_slice(&part);
        }
        let frame = |bytes: &mut Vec<u8>, path: &Path| {
            let value = path.as_os_str().as_encoded_bytes();
            bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
            bytes.extend_from_slice(value);
        };
        frame(&mut handoff, &root.join("package-source/receipt.json"));
        handoff.extend_from_slice(&digest(receipt_bytes));
        // The profile does not exist: recovery must not need to reopen it.
        frame(
            &mut handoff,
            Path::new("/nix/store/00000000000000000000000000000000-collected-profile"),
        );
        handoff.extend_from_slice(&[4; 32]);
        std::fs::write(root.join(HANDOFF), &handoff).unwrap();
        let mut pending = b"TPSFN001".to_vec();
        for part in [
            [1; 32],
            [2; 32],
            [3; 32],
            digest(receipt_bytes),
            [5; 32],
            digest(completion_bytes),
            digest(&handoff),
        ] {
            pending.extend_from_slice(&part);
        }
        frame(&mut pending, &target);
        pending.extend_from_slice(&digest(&pending));
        tidepool_atomic_write::write_durable(&root.join(FINALIZATION), &pending).unwrap();
        let guard = root
            .join("package-source/gc-roots")
            .join(crate::endpoint::hex(&digest(
                target.as_os_str().as_encoded_bytes(),
            )));
        std::os::unix::fs::symlink(&target, &guard).unwrap();
        (owner, guard)
    }

    #[test]
    fn native_retirement_resumes_after_guard_unlink_without_provider_or_profile() {
        let (owner, guard) = pending_retirement();
        // Fail the final durable outcome after the actual guard was removed.
        std::fs::create_dir(owner.path().join(FINALIZED)).unwrap();
        assert!(finish_retirement(owner.path()).is_err());
        assert!(std::fs::symlink_metadata(&guard).is_err());
        std::fs::remove_dir(owner.path().join(FINALIZED)).unwrap();
        assert_eq!(
            crate::frontend::run(vec![
                "--finish-compiler-service-source-retirement".into(),
                owner.path().as_os_str().to_owned(),
            ])
            .unwrap(),
            0
        );
        finish_retirement(owner.path()).unwrap();
        assert!(owner.path().join(FINALIZED).is_file());
    }

    #[test]
    fn native_retirement_refuses_copied_owner_and_missing_pending() {
        let (owner, _guard) = pending_retirement();
        let other = tempfile::tempdir().unwrap();
        for name in [FINALIZATION, HANDOFF, COMPLETION] {
            std::fs::copy(owner.path().join(name), other.path().join(name)).unwrap();
        }
        assert!(finish_retirement(other.path()).is_err());
        std::fs::remove_file(owner.path().join(FINALIZATION)).unwrap();
        assert!(finish_retirement(owner.path()).is_err());
    }

    #[test]
    fn native_retirement_refuses_changed_guard_and_custody_evidence() {
        let (owner, guard) = pending_retirement();
        std::fs::remove_file(&guard).unwrap();
        std::os::unix::fs::symlink("/nix/store/11111111111111111111111111111111-other", &guard)
            .unwrap();
        assert!(finish_retirement(owner.path()).is_err());
        assert!(std::fs::read_link(&guard).is_ok());
        std::fs::remove_file(&guard).unwrap();
        std::os::unix::fs::symlink("/nix/store/00000000000000000000000000000000-python", &guard)
            .unwrap();
        std::fs::write(owner.path().join(COMPLETION), b"unrelated completion").unwrap();
        assert!(finish_retirement(owner.path()).is_err());
        assert!(!owner.path().join(FINALIZED).exists());
    }

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

    fn configured_attempt(owner: &Path) -> (Arc<ConfiguredSource>, ServiceReference) {
        let record = owner.join("source.json");
        let bytes=serde_json::to_vec(&serde_json::json!({
            "schema":2,"kind":"compiler-service-package-source",
            "source_id":crate::endpoint::hex(&[2;32]),"profile_id":crate::endpoint::hex(&[3;32]),
            "provider_guard_store_path":"/nix/store/00000000000000000000000000000000-python",
            "profile_store_path":"/nix/store/11111111111111111111111111111111-profile",
            "profile_sha256":crate::endpoint::hex(&[7;32]),
            "acquisition_roots":["/nix/store/00000000000000000000000000000000-python","/nix/store/11111111111111111111111111111111-profile"]
        })).unwrap();
        std::fs::write(&record, &bytes).unwrap();
        let config = Arc::new(ConfiguredSource {
            python: File::open("/dev/null").unwrap(),
            python_object: "/nix/store/00000000000000000000000000000000-python".into(),
            provider: File::open("/dev/null").unwrap(),
            record,
            record_sha: digest(&bytes),
            source: [2; 32],
            profile: [3; 32],
            launch_sha: [4; 32],
        });
        let attempt = config.attempt([5; 32], owner).unwrap();
        (config, attempt)
    }

    fn provider_frame(outcome: &AcquisitionObservation) -> Vec<u8> {
        let payload = serde_json::to_vec(outcome).unwrap();
        let mut bytes = b"TPSAO001".to_vec();
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend(payload);
        bytes
    }

    #[test]
    fn attempt_is_durable_exact_and_cannot_be_reissued() {
        let owner = tempfile::tempdir().unwrap();
        let (config, attempt) = configured_attempt(owner.path());
        let bytes = std::fs::read(&attempt.path).unwrap();
        assert_eq!(digest(&bytes), attempt.sha256);
        let plan: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(plan["roots"].as_array().unwrap().len(), 2);
        assert!(!owner.path().join("package-source").exists());
        assert!(config.attempt([5; 32], owner.path()).is_err());
        assert_eq!(std::fs::read(attempt.path).unwrap(), bytes);
    }

    #[test]
    fn provider_frame_rejects_every_truncation_trailing_and_oversize() {
        let owner = tempfile::tempdir().unwrap();
        let (_, attempt) = configured_attempt(owner.path());
        let refused = AcquisitionObservation::Refused {
            attempt,
            primary: "exact Nix failure".into(),
            settlement: crate::service::SourceSettlement::PrebootReleased,
            cleanup: vec!["independent cleanup".into()],
            publication: vec!["independent publication".into()],
        };
        let bytes = provider_frame(&refused);
        assert_eq!(decode_provider_frame(&bytes).unwrap(), refused);
        for end in 0..bytes.len() {
            assert!(
                decode_provider_frame(&bytes[..end]).is_err(),
                "accepted prefix {end}"
            );
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(decode_provider_frame(&trailing).is_err());
        let mut huge = b"TPSAO001".to_vec();
        huge.extend_from_slice(&65537u32.to_le_bytes());
        huge.extend(vec![0; 65537]);
        assert!(decode_provider_frame(&huge).is_err());
    }

    #[test]
    fn exact_attempt_and_receipt_survive_missing_handoff_and_profile_failure() {
        let owner = tempfile::tempdir().unwrap();
        let (config, attempt) = configured_attempt(owner.path());
        let plan: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&attempt.path).unwrap()).unwrap();
        let receipt_path = owner.path().join("package-source/receipt.json");
        std::fs::create_dir(receipt_path.parent().unwrap()).unwrap();
        let roots=plan["roots"].as_array().unwrap().iter().map(|item|serde_json::json!({"path":item["path"],"store_path":item["store_path"],"state":"registered"})).collect::<Vec<_>>();
        let receipt_bytes = serde_json::to_vec(&serde_json::json!({
            "schema":2,"kind":"compiler-service-package-acquisition","status":"acquired",
            "service_epoch":crate::endpoint::hex(&[5;32]),
            "source_id":crate::endpoint::hex(&[2;32]),"profile_id":crate::endpoint::hex(&[3;32]),
            "attempt_sha256":crate::endpoint::hex(&attempt.sha256),"source_record_sha256":crate::endpoint::hex(&config.record_sha),
            "provider_guard_store_path":config.python_object,"acquire_count":1,"release_count":0,"roots":roots
        })).unwrap();
        std::fs::write(&receipt_path, &receipt_bytes).unwrap();
        let acquired = AcquisitionObservation::Acquired {
            attempt: attempt.clone(),
            receipt: ServiceReference {
                path: receipt_path,
                sha256: digest(&receipt_bytes),
            },
            profile: ServiceReference {
                path: plan["profile_store_path"].as_str().unwrap().into(),
                sha256: [7; 32],
            },
            publication: vec![],
        };
        config
            .validate_outcome(&acquired, &attempt, [5; 32], owner.path())
            .unwrap();
        std::fs::write(
            owner.path().join("package-provider.stdout"),
            provider_frame(&acquired),
        )
        .unwrap();
        let carrier = config.retained_carrier(owner.path(), [5; 32]).unwrap();
        assert!(!owner.path().join(HANDOFF).exists());
        assert!(config.decode_receipt(&carrier, [5; 32]).is_err());
        assert_eq!(
            config.retained_carrier(owner.path(), [5; 32]).unwrap(),
            carrier
        );
        assert!(config.retained_carrier(owner.path(), [6; 32]).is_err());
        let other = tempfile::tempdir().unwrap();
        assert!(config
            .validate_outcome(&acquired, &attempt, [5; 32], other.path())
            .is_err());
        let mut changed: serde_json::Value = serde_json::from_slice(&receipt_bytes).unwrap();
        changed["roots"][0]["state"] = "planned".into();
        let changed_bytes = serde_json::to_vec(&changed).unwrap();
        if let AcquisitionObservation::Acquired { receipt, .. } = &acquired {
            std::fs::write(&receipt.path, &changed_bytes).unwrap();
        }
        let mut forged = acquired.clone();
        if let AcquisitionObservation::Acquired { receipt, .. } = &mut forged {
            receipt.sha256 = digest(&changed_bytes);
        }
        assert!(config
            .validate_outcome(&forged, &attempt, [5; 32], owner.path())
            .is_err());
    }

    fn source() -> Arc<ServicePackageSource> {
        Arc::new(ServicePackageSource {
            configuration: Arc::new(ConfiguredSource {
                python_object: PathBuf::from("/nix/store/00000000000000000000000000000000-python"),
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
            acquisition: AcquisitionObservation::NotConfigured,
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
