//! Versioned private wire contract for the owning TUI input relay.
//!
//! This is not an app-server API. The host and the already-running TUI use it
//! over the actor-private Unix socket selected during hosted registration.

use serde::{Deserialize, Serialize};

use crate::interactive::{
    InputAdmission, InputEnvelopeError, InputOperationId, InputProducerControlOutcome,
    InputProducerId, InputPurpose, InteractiveInputEnvelope, InteractiveInputError,
    InteractiveInputMode, InteractiveInputTarget, QueueReadyThread,
};
use crate::BackendThreadId;

pub(crate) const INPUT_CONTROL_PROTOCOL_VERSION: u32 = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BindingWire {
    pub protocol_version: u32,
    pub launch_id: String,
    pub instance_id: String,
    pub generation: u64,
    pub nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InputEnvelopeWire {
    pub producer_id: String,
    pub sequence: u64,
    pub purpose: PurposeWire,
    pub mode: ModeWire,
    pub target: TargetWire,
    pub payload: Vec<u8>,
    pub content_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum PurposeWire {
    Bootstrap,
    Assignment,
    RequestUpdate,
    Notification,
    OperatorInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ModeWire {
    QueueOnly,
    StartOrSteer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TargetWire {
    pub conversation: String,
    pub actor: String,
    pub correlation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum InputControlRequest {
    Bind {
        binding: BindingWire,
    },
    Submit {
        binding: BindingWire,
        envelope: InputEnvelopeWire,
    },
    Query {
        binding: BindingWire,
        producer_id: String,
        sequence: u64,
    },
    Withdraw {
        binding: BindingWire,
        producer_id: String,
        sequence: u64,
    },
    Seal {
        binding: BindingWire,
        producer_id: String,
    },
    Acknowledge {
        binding: BindingWire,
        producer_id: String,
        through_sequence: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum OutcomeWire {
    Admitted,
    Dispatching,
    Presented,
    Withdrawn,
    Rejected,
    Unknown,
    Compacted,
    EvidenceUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InputControlResponse {
    pub binding: BindingWire,
    pub outcome: OutcomeWire,
}

fn binding(thread: &QueueReadyThread) -> Result<BindingWire, InteractiveInputError> {
    let value = thread.session_binding().ok_or_else(|| {
        InteractiveInputError::NotSubmitted(
            "bound TUI has not completed the generation/nonce challenge".into(),
        )
    })?;
    Ok(BindingWire {
        protocol_version: INPUT_CONTROL_PROTOCOL_VERSION,
        launch_id: value.launch_id.clone(),
        instance_id: value.instance_id.clone(),
        generation: value.generation.get(),
        nonce: value.nonce.clone(),
    })
}

async fn send(
    thread: &QueueReadyThread,
    request: InputControlRequest,
) -> Result<InputAdmission, InteractiveInputError> {
    let socket = thread.input_control_socket().ok_or_else(|| {
        InteractiveInputError::NotSubmitted("bound TUI did not advertise input control".into())
    })?;
    let expected = binding(thread)?;
    let client = reqwest::Client::builder()
        .unix_socket(socket)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .http1_only()
        .build()
        .map_err(|e| InteractiveInputError::NotSubmitted(e.to_string()))?;
    let response = client
        .post("http://localhost/v1/input/control")
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                InteractiveInputError::NotSubmitted(e.to_string())
            } else {
                InteractiveInputError::Unconfirmed(e.to_string())
            }
        })?;
    if !response.status().is_success() {
        return Err(InteractiveInputError::Unconfirmed(format!(
            "native input control returned HTTP {}",
            response.status()
        )));
    }
    let response: InputControlResponse = response
        .json()
        .await
        .map_err(|e| InteractiveInputError::Unconfirmed(e.to_string()))?;
    if response.binding != expected {
        return Err(InteractiveInputError::Unconfirmed(
            "native input response binding does not match the challenged generation".into(),
        ));
    }
    Ok(match response.outcome {
        OutcomeWire::Admitted => InputAdmission::Admitted,
        OutcomeWire::Dispatching => InputAdmission::Dispatching,
        OutcomeWire::Presented => InputAdmission::Presented,
        OutcomeWire::Withdrawn => InputAdmission::Withdrawn,
        OutcomeWire::Rejected => InputAdmission::Rejected,
        OutcomeWire::Compacted => InputAdmission::Compacted,
        OutcomeWire::Unknown | OutcomeWire::EvidenceUnavailable => InputAdmission::Unknown,
    })
}

pub(super) async fn bind(
    thread: &QueueReadyThread,
) -> Result<InputAdmission, InteractiveInputError> {
    send(
        thread,
        InputControlRequest::Bind {
            binding: binding(thread)?,
        },
    )
    .await
}

pub(super) async fn submit(
    thread: &QueueReadyThread,
    envelope: &InteractiveInputEnvelope,
) -> Result<InputAdmission, InteractiveInputError> {
    send(
        thread,
        InputControlRequest::Submit {
            binding: binding(thread)?,
            envelope: InputEnvelopeWire::from_envelope(envelope),
        },
    )
    .await
}

pub(super) async fn query(
    thread: &QueueReadyThread,
    id: &InputOperationId,
) -> Result<InputAdmission, InteractiveInputError> {
    send(
        thread,
        InputControlRequest::Query {
            binding: binding(thread)?,
            producer_id: id.producer.as_str().to_owned(),
            sequence: id.sequence.get(),
        },
    )
    .await
}

pub(super) async fn withdraw(
    thread: &QueueReadyThread,
    id: &InputOperationId,
) -> Result<InputAdmission, InteractiveInputError> {
    send(
        thread,
        InputControlRequest::Withdraw {
            binding: binding(thread)?,
            producer_id: id.producer.as_str().to_owned(),
            sequence: id.sequence.get(),
        },
    )
    .await
}

pub(super) async fn seal(
    thread: &QueueReadyThread,
    producer: &InputProducerId,
) -> Result<InputProducerControlOutcome, InteractiveInputError> {
    let outcome = send(
        thread,
        InputControlRequest::Seal {
            binding: binding(thread)?,
            producer_id: producer.as_str().to_owned(),
        },
    )
    .await?;
    Ok(match outcome {
        InputAdmission::Withdrawn => InputProducerControlOutcome::Applied,
        InputAdmission::Rejected => InputProducerControlOutcome::Rejected,
        _ => InputProducerControlOutcome::Unknown,
    })
}

pub(super) async fn acknowledge(
    thread: &QueueReadyThread,
    producer: &InputProducerId,
    through_sequence: std::num::NonZeroU64,
) -> Result<InputProducerControlOutcome, InteractiveInputError> {
    let outcome = send(
        thread,
        InputControlRequest::Acknowledge {
            binding: binding(thread)?,
            producer_id: producer.as_str().to_owned(),
            through_sequence: through_sequence.get(),
        },
    )
    .await?;
    Ok(match outcome {
        InputAdmission::Presented => InputProducerControlOutcome::Applied,
        InputAdmission::Rejected => InputProducerControlOutcome::Rejected,
        _ => InputProducerControlOutcome::Unknown,
    })
}

impl InputEnvelopeWire {
    pub(crate) fn from_envelope(value: &InteractiveInputEnvelope) -> Self {
        Self {
            producer_id: value.id().producer.as_str().to_string(),
            sequence: value.id().sequence.get(),
            purpose: match value.purpose() {
                InputPurpose::Bootstrap => PurposeWire::Bootstrap,
                InputPurpose::Assignment => PurposeWire::Assignment,
                InputPurpose::RequestUpdate => PurposeWire::RequestUpdate,
                InputPurpose::Notification => PurposeWire::Notification,
                InputPurpose::OperatorInput => PurposeWire::OperatorInput,
            },
            mode: match value.mode() {
                InteractiveInputMode::QueueOnly => ModeWire::QueueOnly,
                InteractiveInputMode::StartOrSteer => ModeWire::StartOrSteer,
            },
            target: TargetWire {
                conversation: value.target().conversation.0.clone(),
                actor: value.target().actor.clone(),
                correlation: value.target().correlation.clone(),
            },
            payload: value.bytes().to_vec(),
            content_digest: encode_digest(value.digest()),
        }
    }

    /// Recompute the canonical digest before this operation can cross native admission.
    pub(crate) fn validate(self) -> Result<InteractiveInputEnvelope, InputControlWireError> {
        let sequence =
            std::num::NonZeroU64::new(self.sequence).ok_or(InputControlWireError::ZeroSequence)?;
        let digest = decode_digest(&self.content_digest)?;
        Ok(InteractiveInputEnvelope::from_persisted(
            InputOperationId {
                producer: InputProducerId::new(self.producer_id)?,
                sequence,
            },
            match self.purpose {
                PurposeWire::Bootstrap => InputPurpose::Bootstrap,
                PurposeWire::Assignment => InputPurpose::Assignment,
                PurposeWire::RequestUpdate => InputPurpose::RequestUpdate,
                PurposeWire::Notification => InputPurpose::Notification,
                PurposeWire::OperatorInput => InputPurpose::OperatorInput,
            },
            match self.mode {
                ModeWire::QueueOnly => InteractiveInputMode::QueueOnly,
                ModeWire::StartOrSteer => InteractiveInputMode::StartOrSteer,
            },
            InteractiveInputTarget {
                conversation: BackendThreadId(self.target.conversation),
                actor: self.target.actor,
                correlation: self.target.correlation,
            },
            self.payload,
            digest,
        )?)
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InputControlWireError {
    #[error("input sequence must be nonzero")]
    ZeroSequence,
    #[error("input digest must be exactly 64 lowercase hexadecimal characters")]
    InvalidDigest,
    #[error(transparent)]
    Envelope(#[from] InputEnvelopeError),
}

fn encode_digest(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(char::from(HEX[(byte >> 4) as usize]));
        encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    encoded
}

fn decode_digest(encoded: &str) -> Result<[u8; 32], InputControlWireError> {
    if encoded.len() != 64
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(InputControlWireError::InvalidDigest);
    }
    let mut digest = [0; 32];
    for (index, output) in digest.iter_mut().enumerate() {
        *output = u8::from_str_radix(&encoded[index * 2..index * 2 + 2], 16)
            .map_err(|_| InputControlWireError::InvalidDigest)?;
    }
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn challenged_thread(socket: std::path::PathBuf) -> QueueReadyThread {
        QueueReadyThread::new(BackendThreadId("thread".into()))
            .with_input_control(Some(socket))
            .with_challenged_session_binding(Some(crate::InteractiveSessionBinding {
                launch_id: "launch-1".into(),
                instance_id: "instance-2".into(),
                generation: std::num::NonZeroU64::new(7).unwrap(),
                nonce: "nonce-3".into(),
            }))
    }

    async fn serve_once(
        listener: tokio::net::UnixListener,
        response: Option<InputControlResponse>,
    ) -> serde_json::Value {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let body = loop {
            let mut chunk = [0; 4096];
            let size = stream.read(&mut chunk).await.unwrap();
            assert!(size > 0, "request ended before its complete body");
            bytes.extend_from_slice(&chunk[..size]);
            let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let header = std::str::from_utf8(&bytes[..end]).unwrap();
            let length: usize = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + length {
                break bytes[end + 4..end + 4 + length].to_vec();
            }
        };
        if let Some(response) = response {
            let body = serde_json::to_vec(&response).unwrap();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            stream.write_all(&body).await.unwrap();
        }
        drop(stream);
        serde_json::from_slice(&body).unwrap()
    }

    fn bind_response(generation: u64, outcome: OutcomeWire) -> InputControlResponse {
        InputControlResponse {
            binding: BindingWire {
                protocol_version: INPUT_CONTROL_PROTOCOL_VERSION,
                launch_id: "launch-1".into(),
                instance_id: "instance-2".into(),
                generation,
                nonce: "nonce-3".into(),
            },
            outcome,
        }
    }

    fn golden_envelope() -> InteractiveInputEnvelope {
        InteractiveInputEnvelope::new(
            InputOperationId {
                producer: InputProducerId::new("run-7/inbox-2/actor-3.1".into()).unwrap(),
                sequence: std::num::NonZeroU64::new(9).unwrap(),
            },
            InputPurpose::Assignment,
            InteractiveInputMode::QueueOnly,
            InteractiveInputTarget {
                conversation: BackendThreadId("00000000-0000-0000-0000-000000000004".into()),
                actor: "actor-3.1".into(),
                correlation: Some("request-5".into()),
            },
            b"hello".to_vec(),
        )
        .unwrap()
    }

    #[test]
    fn purpose_is_preserved_but_not_mistaken_for_digest_equality() {
        let original = golden_envelope();
        let changed = InteractiveInputEnvelope::new(
            original.id().clone(),
            InputPurpose::Notification,
            original.mode(),
            original.target().clone(),
            original.bytes().to_vec(),
        )
        .unwrap();
        assert_eq!(original.digest(), changed.digest());
        assert_ne!(original, changed);
        assert_ne!(
            InputEnvelopeWire::from_envelope(&original).purpose,
            InputEnvelopeWire::from_envelope(&changed).purpose
        );
    }

    #[tokio::test]
    async fn submission_without_challenged_generation_is_not_submitted() {
        let thread = QueueReadyThread::new(BackendThreadId("thread".into()));
        assert!(matches!(
            submit(&thread, &golden_envelope()).await,
            Err(InteractiveInputError::NotSubmitted(_))
        ));
    }

    #[tokio::test]
    async fn bind_uses_the_existing_challenge_once_and_returns_native_admitted() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("input.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let request = serve_once(listener, Some(bind_response(7, OutcomeWire::Admitted))).await;
            assert_eq!(
                request,
                serde_json::json!({
                    "operation": "bind",
                    "binding": {
                        "protocolVersion": 4,
                        "launchId": "launch-1",
                        "instanceId": "instance-2",
                        "generation": 7,
                        "nonce": "nonce-3"
                    }
                })
            );
        });

        assert_eq!(
            bind(&challenged_thread(socket)).await.unwrap(),
            InputAdmission::Admitted
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn bind_unavailable_socket_is_not_submitted() {
        let directory = tempfile::tempdir().unwrap();
        let result = bind(&challenged_thread(directory.path().join("missing.sock"))).await;
        assert!(matches!(
            result,
            Err(InteractiveInputError::NotSubmitted(_))
        ));
    }

    #[tokio::test]
    async fn bind_lost_ack_and_generation_mismatch_are_unconfirmed() {
        for (response, expected_detail) in [
            (None, None),
            (
                Some(bind_response(8, OutcomeWire::Admitted)),
                Some("does not match the challenged generation"),
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let socket = directory.path().join("input.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let server = tokio::spawn(async move {
                let request = serve_once(listener, response).await;
                assert_eq!(request["operation"], "bind");
            });
            let Err(InteractiveInputError::Unconfirmed(detail)) =
                bind(&challenged_thread(socket)).await
            else {
                panic!("accepted an unconfirmed native bind");
            };
            if let Some(expected) = expected_detail {
                assert!(detail.contains(expected), "{detail}");
            }
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn bind_exposes_non_admitted_native_outcome_for_the_host_to_reject() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("input.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            serve_once(
                listener,
                Some(bind_response(7, OutcomeWire::EvidenceUnavailable)),
            )
            .await
        });
        assert_eq!(
            bind(&challenged_thread(socket)).await.unwrap(),
            InputAdmission::Unknown
        );
        assert_eq!(server.await.unwrap()["operation"], "bind");
    }

    #[tokio::test]
    async fn producer_controls_require_the_challenged_native_binding() {
        let thread = QueueReadyThread::new(BackendThreadId("thread".into()));
        let envelope = golden_envelope();
        assert!(matches!(
            withdraw(&thread, envelope.id()).await,
            Err(InteractiveInputError::NotSubmitted(_))
        ));
        assert!(matches!(
            seal(&thread, &envelope.id().producer).await,
            Err(InteractiveInputError::NotSubmitted(_))
        ));
        assert!(matches!(
            acknowledge(&thread, &envelope.id().producer, envelope.id().sequence).await,
            Err(InteractiveInputError::NotSubmitted(_))
        ));
    }

    #[test]
    fn golden_envelope_round_trips_and_rejects_changed_mode() {
        let wire = InputEnvelopeWire::from_envelope(&golden_envelope());
        let json = serde_json::to_string(&wire).unwrap();
        assert_eq!(json, GOLDEN_JSON);
        assert_eq!(wire.clone().validate().unwrap(), golden_envelope());
        let mut changed = wire;
        changed.mode = ModeWire::StartOrSteer;
        assert!(matches!(
            changed.validate(),
            Err(InputControlWireError::Envelope(
                InputEnvelopeError::DigestMismatch
            ))
        ));
    }

    const GOLDEN_JSON: &str = "{\"producerId\":\"run-7/inbox-2/actor-3.1\",\"sequence\":9,\"purpose\":\"assignment\",\"mode\":\"queueOnly\",\"target\":{\"conversation\":\"00000000-0000-0000-0000-000000000004\",\"actor\":\"actor-3.1\",\"correlation\":\"request-5\"},\"payload\":[104,101,108,108,111],\"contentDigest\":\"282cff748dac084436730b60201fc26b7b9b4f2ccfbbbf4cbd6966e7fc4d5cd9\"}";

    #[test]
    fn compacted_outcome_matches_native_wire_vector() {
        let json = r#"{"binding":{"protocolVersion":4,"launchId":"launch-1","instanceId":"instance-2","generation":7,"nonce":"nonce-3"},"outcome":"compacted"}"#;
        let response: InputControlResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.outcome, OutcomeWire::Compacted);
        assert_eq!(serde_json::to_string(&response).unwrap(), json);
    }
}
