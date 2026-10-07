//! Framing laws against independently assembled reply bytes and byte offsets.
//! These tests exercise the decoder and accepted-transaction client, not a GHC
//! worker or daemon supervision. A failed accepted reply is never replayed.

use super::*;
use proptest::prelude::*;
use std::net::Shutdown;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Reply {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn replies() -> impl Strategy<Value = Reply> {
    (
        any::<i32>(),
        proptest::collection::vec(any::<u8>(), 0..65),
        proptest::collection::vec(any::<u8>(), 0..65),
    )
        .prop_map(|(code, stdout, stderr)| Reply {
            code,
            stdout,
            stderr,
        })
}

fn read_widths() -> impl Strategy<Value = Vec<usize>> {
    proptest::collection::vec(1usize..18, 1..9)
}

// Deliberately does not call the production writer or frame helper. The model
// is the wire contract: signed status, stdout frame, then stderr frame.
fn reply_bytes(reply: &Reply) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&reply.code.to_le_bytes());
    bytes.extend_from_slice(&(reply.stdout.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&reply.stdout);
    bytes.extend_from_slice(&(reply.stderr.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&reply.stderr);
    bytes
}

#[derive(Clone, Copy, Debug)]
enum ReadFault {
    End,
    InterruptedOnce,
    Error(io::ErrorKind),
}

struct PartitionedRead<'a> {
    bytes: &'a [u8],
    widths: &'a [usize],
    position: usize,
    reads: usize,
    fault: Option<(usize, ReadFault)>,
    fault_observed: bool,
}

impl<'a> PartitionedRead<'a> {
    fn new(bytes: &'a [u8], widths: &'a [usize], fault: Option<(usize, ReadFault)>) -> Self {
        assert!(!widths.is_empty() && widths.iter().all(|width| *width > 0));
        Self {
            bytes,
            widths,
            position: 0,
            reads: 0,
            fault,
            fault_observed: false,
        }
    }
}

impl Read for PartitionedRead<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if let Some((at, fault)) = self.fault {
            if self.position == at && !self.fault_observed {
                self.fault_observed = true;
                return match fault {
                    ReadFault::End => Ok(0),
                    ReadFault::InterruptedOnce => Err(io::ErrorKind::Interrupted.into()),
                    ReadFault::Error(kind) => Err(kind.into()),
                };
            }
        }
        let width = self.widths[self.reads % self.widths.len()];
        self.reads += 1;
        let mut count = buffer
            .len()
            .min(width)
            .min(self.bytes.len() - self.position);
        if let Some((at, _)) = self.fault {
            if !self.fault_observed {
                count = count.min(at - self.position);
            }
        }
        buffer[..count].copy_from_slice(&self.bytes[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

fn property_config() -> proptest::test_runner::Config {
    let mut config = proptest::test_runner::Config::default();
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(path),
        ));
    }
    config
}

proptest! {
    #![proptest_config(property_config())]

    #[test]
    fn response_sequences_preserve_frames_across_read_partitions(
        replies in proptest::collection::vec(replies(), 2..7),
        widths in read_widths(),
        interrupted_byte in any::<usize>(),
    ) {
        let wire: Vec<u8> = replies.iter().flat_map(reply_bytes).collect();
        for fault in [None, Some((interrupted_byte % wire.len(), ReadFault::InterruptedOnce))] {
            let mut input = PartitionedRead::new(&wire, &widths, fault);
            let mut expected_position = 0;
            for reply in &replies {
                let mut encoded = Vec::new();
                write_response(&mut encoded, reply.code, &reply.stdout, &reply.stderr).unwrap();
                prop_assert_eq!(encoded, reply_bytes(reply));
                let actual = decode_response(&mut input).unwrap();
                prop_assert_eq!(actual, (reply.code, reply.stdout.clone(), reply.stderr.clone()));
                expected_position += 12 + reply.stdout.len() + reply.stderr.len();
                prop_assert_eq!(input.position, expected_position,
                    "a reply must consume exactly its bytes, preserving the following reply");
            }
            prop_assert_eq!(input.position, wire.len());
            prop_assert_eq!(input.fault_observed, fault.is_some());
        }
    }

    #[test]
    fn every_proper_response_prefix_refuses_completion(reply in replies(), widths in read_widths()) {
        let wire = reply_bytes(&reply);
        // Cover every header and body cut for each constructed valid reply.
        for cut in 0..wire.len() {
            let mut input = PartitionedRead::new(&wire[..cut], &widths, None);
            let error = decode_response(&mut input).unwrap_err();
            prop_assert!(matches!(error, DaemonError::IncompleteResponse),
                "cut {cut} of {} produced {error:?}", wire.len());
            prop_assert_eq!(input.position, cut);
        }
    }

    #[test]
    fn zero_read_cannot_complete_a_partial_reply(
        replies in proptest::collection::vec(replies(), 2..5),
        widths in read_widths(),
        stop_byte in any::<usize>(),
    ) {
        let wire: Vec<u8> = replies.iter().flat_map(reply_bytes).collect();
        let stop = stop_byte % wire.len();
        let mut input = PartitionedRead::new(&wire, &widths, Some((stop, ReadFault::End)));
        let mut boundary = 0;
        for reply in &replies {
            boundary += 12 + reply.stdout.len() + reply.stderr.len();
            let result = decode_response(&mut input);
            if boundary <= stop {
                prop_assert_eq!(result.unwrap(),
                    (reply.code, reply.stdout.clone(), reply.stderr.clone()));
            } else {
                prop_assert!(matches!(result, Err(DaemonError::IncompleteResponse)),
                    "zero read at {stop} inside reply ending at {boundary} must be EOF");
                prop_assert_eq!(input.position, stop);
                prop_assert!(input.fault_observed);
                break;
            }
        }
    }

    #[test]
    fn noninterrupted_read_failures_preserve_error_and_consumption(
        reply in replies(), widths in read_widths(), failure_byte in any::<usize>(),
        kind_index in 0usize..3,
    ) {
        let wire = reply_bytes(&reply);
        let failure = failure_byte % wire.len();
        let kind = [io::ErrorKind::ConnectionReset, io::ErrorKind::TimedOut,
            io::ErrorKind::InvalidData][kind_index];
        let mut input = PartitionedRead::new(&wire, &widths, Some((failure, ReadFault::Error(kind))));
        let result = decode_response(&mut input);
        prop_assert!(matches!(&result, Err(DaemonError::Io(error)) if error.kind() == kind),
            "non-Interrupted failure must propagate unchanged: {result:?}");
        prop_assert_eq!(input.position, failure);
        prop_assert!(input.fault_observed);
    }

    #[test]
    fn oversized_second_frame_refuses_before_reading_body(
        code in any::<i32>(), stdout in proptest::collection::vec(any::<u8>(), 0..65),
        overflow in 1u32..65, widths in read_widths(),
    ) {
        let remaining = MAX_RESPONSE_PAYLOAD_BYTES - stdout.len() as u32;
        let declared = remaining + overflow;
        let mut wire = reply_bytes(&Reply { code, stdout: stdout.clone(), stderr: Vec::new() });
        let header_end = wire.len();
        wire[header_end - 4..].copy_from_slice(&declared.to_le_bytes());
        wire.extend_from_slice(b"body must remain unread");
        let mut input = PartitionedRead::new(&wire, &widths, None);
        let result = decode_response(&mut input);
        prop_assert!(matches!(result, Err(DaemonError::ResponseTooLarge {
            declared: actual, remaining: available,
        }) if actual == u64::from(declared) && available == u64::from(remaining)),
            "aggregate stdout/stderr budget must be enforced before the second body");
        prop_assert_eq!(input.position, header_end);
    }

    #[test]
    fn accepted_partial_response_closes_without_replay_and_fresh_transaction_succeeds(
        failed in replies(), fresh in replies(), cut_selector in any::<usize>(),
    ) {
        let wire = reply_bytes(&failed);
        let cut = cut_selector % wire.len();
        let (client, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        peer.write_all(&wire[..cut]).unwrap();
        peer.shutdown(Shutdown::Write).unwrap();
        let mut transaction = DaemonTransaction::for_test(client);
        let error = execute_transaction_request(&mut transaction, Path::new("/fixture"), &[]).unwrap_err();
        prop_assert!(error.was_accepted());
        prop_assert!(!error.permits_rebind());
        prop_assert!(matches!(error, DaemonError::AfterAcceptance(inner)
            if matches!(*inner, DaemonError::IncompleteResponse)),
            "a prefix response after admission must remain indeterminate");
        prop_assert_eq!(transaction.next_request, RequestOrdinal(2));
        drop(transaction);
        let mut emitted = Vec::new();
        peer.read_to_end(&mut emitted).unwrap();
        // One command and one independently framed cwd/empty argv list. This
        // proves the client issued once and its owned stream closed on drop.
        let mut expected = vec![TRANSACTION_REQUEST];
        expected.extend_from_slice(&8u32.to_le_bytes());
        expected.extend_from_slice(b"/fixture");
        expected.extend_from_slice(&0u32.to_le_bytes());
        prop_assert_eq!(&emitted, &expected);

        let (client, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        peer.write_all(&reply_bytes(&fresh)).unwrap();
        peer.shutdown(Shutdown::Write).unwrap();
        let mut transaction = DaemonTransaction::for_test(client);
        let output = execute_transaction_request(&mut transaction, Path::new("/fixture"), &[]).unwrap();
        prop_assert_eq!(output.status.code(), Some(i32::from(fresh.code as u8)));
        prop_assert_eq!(output.stdout, fresh.stdout);
        prop_assert_eq!(output.stderr, fresh.stderr);
        prop_assert_eq!(transaction.next_request, RequestOrdinal(2));
        drop(transaction);
        let mut emitted = Vec::new();
        peer.read_to_end(&mut emitted).unwrap();
        prop_assert_eq!(&emitted, &expected);
    }
}

#[test]
fn fixed_read_fault_support_covers_every_byte_boundary() {
    let first = Reply {
        code: -7,
        stdout: vec![0, 255, 1],
        stderr: vec![2, 0, 3, 255],
    };
    let second = Reply {
        code: 19,
        stdout: Vec::new(),
        stderr: vec![8],
    };
    let wire: Vec<u8> = [&first, &second]
        .into_iter()
        .flat_map(reply_bytes)
        .collect();
    let first_end = reply_bytes(&first).len();
    for at in 0..wire.len() {
        let mut input =
            PartitionedRead::new(&wire, &[1, 4, 2], Some((at, ReadFault::InterruptedOnce)));
        assert_eq!(
            decode_response(&mut input).unwrap(),
            (first.code, first.stdout.clone(), first.stderr.clone())
        );
        assert_eq!(input.position, first_end);
        assert_eq!(
            decode_response(&mut input).unwrap(),
            (second.code, second.stdout.clone(), second.stderr.clone())
        );
        assert!(input.fault_observed);
        assert_eq!(input.position, wire.len());

        for fault in [
            ReadFault::End,
            ReadFault::Error(io::ErrorKind::ConnectionReset),
        ] {
            let mut input = PartitionedRead::new(&wire, &[1, 4, 2], Some((at, fault)));
            if at >= first_end {
                assert_eq!(
                    decode_response(&mut input).unwrap(),
                    (first.code, first.stdout.clone(), first.stderr.clone())
                );
                assert_eq!(input.position, first_end);
            }
            let error = decode_response(&mut input).unwrap_err();
            match fault {
                ReadFault::End => assert!(matches!(error, DaemonError::IncompleteResponse)),
                ReadFault::Error(kind) => {
                    assert!(matches!(error, DaemonError::Io(error) if error.kind() == kind));
                }
                ReadFault::InterruptedOnce => unreachable!(),
            }
            assert!(input.fault_observed);
            assert_eq!(input.position, at);
        }
    }
}
