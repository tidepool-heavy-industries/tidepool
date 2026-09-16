mod frontier;
mod interpret;
mod probes;
mod scenarios;
mod simulations;
mod transport;
mod worlds;

use clap::{Parser, Subcommand, ValueEnum};
use probes::Probe;
use serde::Serialize;
use serde_json::Value;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
    process::ExitCode,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use transport::{bearer, Exchange, Transport};

#[derive(Parser)]
#[command(
    about = "TypeSafe contract experiments; no inference unless run is explicitly invoked",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run adversarial Jev-shaped boundary cases; no external actions execute.
    Frontier {
        #[arg(long)]
        output_dir: PathBuf,
        #[arg(long, default_value = "jev-latest")]
        model: String,
    },
    /// Run a bounded synthetic notebook decision chain; no shell commands execute.
    Simulate {
        #[arg(value_enum)]
        scenario: simulations::Scenario,
        #[arg(long)]
        output_dir: PathBuf,
        #[arg(long, default_value = "jev-latest")]
        model: String,
    },
    /// List synthetic experiments; no network.
    List,
    /// Print an experiment's exact JSON request; no network or key required.
    Show {
        #[arg(value_enum)]
        probe: Probe,
        #[arg(long, default_value = "jev-latest")]
        model: String,
    },
    /// Make exactly one authenticated evaluation attempt and save evidence.
    Run {
        #[arg(value_enum)]
        probe: Probe,
        #[arg(long, default_value = "jev-latest")]
        model: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = 15000, value_parser = clap::value_parser!(u64).range(1..))]
        timeout_ms: u64,
    },
    /// Fetch public OpenAPI once, without authentication or inference.
    Snapshot {
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Serialize)]
struct Evidence {
    format_version: u32,
    started_unix_ms: u128,
    harness_blake3: String,
    endpoint: &'static str,
    probe: Option<String>,
    request: Option<Value>,
    exchange: Exchange,
    body_credentials_redacted: bool,
    response_json: Option<Value>,
    interpretation: Option<interpret::Interpretation>,
}

fn harness_hash() -> String {
    let mut hash = blake3::Hasher::new();
    for source in [
        include_str!("main.rs"),
        include_str!("frontier.rs"),
        include_str!("probes.rs"),
        include_str!("scenarios.rs"),
        include_str!("simulations.rs"),
        include_str!("worlds.rs"),
        include_str!("transport.rs"),
        include_str!("interpret.rs"),
        include_str!("../Cargo.toml"),
        include_str!("../../Cargo.lock"),
    ] {
        hash.update(&(source.len() as u64).to_le_bytes());
        hash.update(source.as_bytes());
    }
    hash.finalize().to_hex().to_string()
}

fn new_evidence_file(path: &PathBuf) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

// Preserve bytes except any literal/JSON-escaped copy of the credential echoed
// by a server. A flag distinguishes sanitized evidence from an exact body.
fn redact(body: &mut Vec<u8>, key: &str) -> bool {
    let quoted = serde_json::to_string(key).expect("strings serialize");
    let mut changed = false;
    for needle in [key.as_bytes(), &quoted.as_bytes()[1..quoted.len() - 1]] {
        if needle.is_empty() {
            continue;
        }
        let mut result = Vec::new();
        let mut pos = 0;
        while pos < body.len() {
            if body[pos..].starts_with(needle) {
                result.extend_from_slice(b"[REDACTED]");
                pos += needle.len();
                changed = true;
            } else {
                result.push(body[pos]);
                pos += 1;
            }
        }
        *body = result;
    }
    changed
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match execute(Cli::parse()).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

async fn execute(cli: Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let (endpoint, probe, request, key, output, timeout) = match cli.command {
        Command::Frontier { output_dir, model } => {
            return frontier::run(output_dir, &model).await;
        }
        Command::Simulate {
            scenario,
            output_dir,
            model,
        } => {
            return simulations::run(scenario, output_dir, &model).await;
        }
        Command::List => {
            for probe in Probe::value_variants() {
                println!("{}", probe.name());
            }
            return Ok(ExitCode::SUCCESS);
        }
        Command::Show { probe, model } => {
            println!("{}", serde_json::to_string_pretty(&probe.request(&model))?);
            return Ok(ExitCode::SUCCESS);
        }
        Command::Run {
            probe,
            model,
            output,
            timeout_ms,
        } => {
            let key = std::env::var("TYPESAFE_API_KEY").map_err(|_| {
                "Set TYPESAFE_API_KEY in the process environment before running live probes"
            })?;
            bearer(&key)?;
            (
                "https://api.typesafe.ai/v1/systemone",
                Some(probe.name()),
                Some(probe.request(&model)),
                Some(key),
                output,
                Duration::from_millis(timeout_ms),
            )
        }
        Command::Snapshot { output } => (
            "https://api.typesafe.ai/openapi.json",
            None,
            None,
            None,
            output,
            Duration::from_secs(15),
        ),
    };
    let (_, success) = capture_exchange(endpoint, probe, request, key, output, timeout).await?;
    Ok(if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

async fn capture_exchange(
    endpoint: &'static str,
    probe: Option<String>,
    request: Option<Value>,
    key: Option<String>,
    output: PathBuf,
    timeout: Duration,
) -> Result<(Evidence, bool), Box<dyn std::error::Error>> {
    let client = Transport::new(timeout)?;
    // Reserve output before spending a request; refuse to overwrite evidence.
    let mut file = new_evidence_file(&output)?;
    let started_unix_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let mut exchange = client
        .exchange(
            endpoint,
            request.as_ref(),
            key.as_deref().map(bearer).transpose()?,
        )
        .await;
    let body_credentials_redacted = key
        .as_deref()
        .is_some_and(|key| redact(&mut exchange.body, key));
    let http_success = exchange.failure.is_none()
        && exchange
            .status
            .is_some_and(|code| (200..300).contains(&code));
    let response_json = serde_json::from_slice(&exchange.body).ok();
    let interpretation = request
        .as_ref()
        .filter(|_| http_success)
        .map(|request| interpret::interpret(request, &exchange.body));
    let findings = interpretation.as_ref().map_or(0, |r| r.findings.len());
    let status = exchange.status;
    let evidence = Evidence {
        format_version: 1,
        started_unix_ms,
        harness_blake3: harness_hash(),
        endpoint,
        probe,
        request,
        exchange,
        body_credentials_redacted,
        response_json,
        interpretation,
    };
    serde_json::to_writer_pretty(&mut file, &evidence)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    println!(
        "Saved {} (HTTP {:?}, {} interpretation findings)",
        output.display(),
        status,
        findings
    );
    // Rejection is evidence, but must not look like successful inference.
    Ok((evidence, http_success && findings == 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_does_not_overwrite_an_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("run.json");
        let mut file = new_evidence_file(&path).unwrap();
        file.write_all(b"original").unwrap();
        assert_eq!(
            new_evidence_file(&path).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
    }

    #[test]
    fn echoed_credentials_are_removed_before_decoding_or_saving() {
        let mut body = br#"{"echo":"test-secret"}"#.to_vec();
        assert!(redact(&mut body, "test-secret"));
        assert_eq!(body, br#"{"echo":"[REDACTED]"}"#);
    }

    #[test]
    fn all_named_probes_have_a_valid_json_round_trip() {
        for probe in Probe::value_variants() {
            let body = probe.request("test-model");
            assert_eq!(
                serde_json::from_slice::<Value>(&serde_json::to_vec(&body).unwrap()).unwrap(),
                body
            );
        }
        let openapi: Value =
            serde_json::from_str(include_str!("../fixtures/openapi.json")).unwrap();
        assert!(openapi["paths"].get("/v1/systemone").is_some());
    }
}
