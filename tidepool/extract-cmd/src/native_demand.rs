//! The compiler's request-local native dependency exchange. Payload interpretation
//! belongs to the held compiler input owner; these coordinates only correlate IO.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use crate::CompilerTransactionCancellation;

pub(crate) const REQUEST: u8 = 6;
pub(crate) const DEMAND: u8 = 4;
pub(crate) const REPLY: u8 = 5;
const MAGIC: &[u8; 8] = b"TPNDX001";
pub(crate) const PAYLOAD_BUDGET: u32 = crate::daemon::MAX_RESPONSE_PAYLOAD_BYTES;
// Demand tag/header/stage/sequence/length, plus the reply outcome byte.
pub(crate) const ROUND_FRAME_BYTES: usize = 2 * (1 + 144 + 8 + 8 + 4) + 1;

/// Full semantic owner identities supplied by the actual held input owner.
/// Possession of these correlation bytes grants no compiler authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeDemandOwner {
    pub admission: [u8; 32],
    pub world: [u8; 32],
    pub inputs: [u8; 32],
}

/// One compiler-observed batch at an actual inference stage.
#[derive(Debug)]
pub struct NativeDemand {
    pub owner: NativeDemandOwner,
    pub stage: u64,
    pub sequence: u64,
    pub payload: Vec<u8>,
}

/// Semantic refusal resumes the handler's normal failed-result/END path.
/// An IO failure instead poisons accepted transport and cannot authorize replay.
#[derive(Debug)]
pub enum NativeReply {
    Resolved(Vec<u8>),
    Refused(Vec<u8>),
}

/// Implemented by the request's existing held native/input owner. Resolution
/// cannot execute effects or start nested compilation, and must checkpoint its
/// bounded traversal through `control` before expensive work.
pub trait NativeDemandResolver {
    fn resolve_native_demand(
        &mut self,
        demand: &NativeDemand,
        control: &NativeDemandControl,
    ) -> io::Result<NativeReply>;
}

/// Owns semantic completion of one native-demand execution. The session is
/// consumed after transport returns, including every normal admission failure.
/// Transaction END, input custody and unwinding remain with their existing owner.
pub trait NativeDemandSession: NativeDemandResolver + Sized {
    type Outcome;

    fn owner(&self) -> NativeDemandOwner;
    fn complete(self, execution: Result<crate::ExtractRun, crate::SpawnError>) -> Self::Outcome;
}

/// The same absolute admitted deadline and cancellation edge throughout an
/// exchange, including host-side semantic resolution.
#[derive(Clone, Debug)]
pub struct NativeDemandControl {
    deadline: MonotonicDeadline,
    cancellation: Option<CompilerTransactionCancellation>,
}

impl NativeDemandControl {
    pub fn checkpoint(&self) -> io::Result<()> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(CompilerTransactionCancellation::is_cancelled)
        {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "native demand cancelled",
            ));
        }
        if monotonic_nanos()? >= self.deadline.0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native demand request deadline exceeded",
            ));
        }
        Ok(())
    }

    pub fn remaining(&self) -> io::Result<Duration> {
        self.checkpoint()?;
        Ok(Duration::from_nanos(
            self.deadline.0.saturating_sub(monotonic_nanos()?),
        ))
    }

    pub(crate) fn instant(&self) -> io::Result<Instant> {
        Ok(Instant::now() + self.remaining()?)
    }

    pub(crate) fn read<'a, R: Read + AsRawFd>(
        &'a self,
        reader: &'a mut R,
    ) -> io::Result<ControlledReader<'a, R>> {
        self.checkpoint()?;
        Ok(ControlledReader {
            reader,
            control: self,
        })
    }

    pub(crate) fn writer<'a, W: Write + AsRawFd>(
        &'a self,
        writer: &'a mut W,
    ) -> io::Result<ControlledWriter<'a, W>> {
        self.checkpoint()?;
        let fd = writer.as_raw_fd();
        // SAFETY: the caller exclusively owns this request's writer throughout
        // the guard; fcntl only observes/updates this retained descriptor.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ControlledWriter {
            writer,
            control: self,
            flags,
        })
    }
}

/// A blocking pipe write may outlive its last checkpoint even after POLLOUT.
/// Nonblocking writes keep the original deadline observable for large replies.
pub(crate) struct ControlledWriter<'a, W: AsRawFd> {
    writer: &'a mut W,
    control: &'a NativeDemandControl,
    flags: libc::c_int,
}

impl<W: AsRawFd> ControlledWriter<'_, W> {
    fn ready(&self) -> io::Result<()> {
        loop {
            self.control.checkpoint()?;
            let timeout = self
                .control
                .remaining()?
                .min(Duration::from_millis(50))
                .as_millis()
                .max(1) as libc::c_int;
            let mut descriptor = libc::pollfd {
                fd: self.writer.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            // SAFETY: one initialized pollfd references the writer retained by
            // this guard; readiness never grants authority beyond that writer.
            let ready = unsafe { libc::poll(&mut descriptor, 1, timeout) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            self.control.checkpoint()?;
            if ready == 0 {
                continue;
            }
            if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "native exchange writer closed",
                ));
            }
            return Ok(());
        }
    }
}

impl<W: Write + AsRawFd> Write for ControlledWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            self.ready()?;
            match self.writer.write(bytes) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue
                }
                result => {
                    self.control.checkpoint()?;
                    return result;
                }
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        loop {
            self.control.checkpoint()?;
            match self.writer.flush() {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.ready()?,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => {
                    self.control.checkpoint()?;
                    return result;
                }
            }
        }
    }
}

impl<W: AsRawFd> Drop for ControlledWriter<'_, W> {
    fn drop(&mut self) {
        // SAFETY: this guard still exclusively holds the original writer.
        if unsafe { libc::fcntl(self.writer.as_raw_fd(), libc::F_SETFL, self.flags) } < 0 {
            tracing::warn!(error = %io::Error::last_os_error(), "could not restore compiler writer flags");
        }
    }
}

pub(crate) struct ControlledReader<'a, R> {
    reader: &'a mut R,
    control: &'a NativeDemandControl,
}

/// While the client resolves a demand the worker must remain suspended. A
/// terminal response or another demand before the reply is a poisoned stream.
pub(crate) struct ReplyReader<'a, R> {
    reader: &'a mut R,
    control: &'a NativeDemandControl,
    worker: std::os::fd::RawFd,
}

pub(crate) fn ensure_worker_quiet(worker: std::os::fd::RawFd) -> io::Result<()> {
    let mut descriptor = libc::pollfd {
        fd: worker,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: the request owner retains the worker stdout descriptor while this
    // nonblocking observation runs; this does not consume its response stream.
    let ready = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if ready < 0 {
        return Err(io::Error::last_os_error());
    }
    if ready != 0 {
        return Err(invalid("worker emitted output before native reply"));
    }
    Ok(())
}

impl NativeDemandControl {
    pub(crate) fn read_reply<'a, R: Read + AsRawFd>(
        &'a self,
        reader: &'a mut R,
        worker: std::os::fd::RawFd,
    ) -> ReplyReader<'a, R> {
        ReplyReader {
            reader,
            control: self,
            worker,
        }
    }
}

impl<R: Read + AsRawFd> Read for ReplyReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        ensure_worker_quiet(self.worker)?;
        crate::process::read_exact_until(self.reader, bytes, self.control.instant()?, || {
            self.control.checkpoint().is_err() || ensure_worker_quiet(self.worker).is_err()
        })
        .map_err(|error| {
            if let Err(primary) = self
                .control
                .checkpoint()
                .and_then(|()| ensure_worker_quiet(self.worker))
            {
                return primary;
            }
            if error.kind() == io::ErrorKind::Interrupted {
                io::Error::new(io::ErrorKind::ConnectionAborted, error)
            } else {
                error
            }
        })?;
        self.control.checkpoint()?;
        ensure_worker_quiet(self.worker)?;
        Ok(bytes.len())
    }
}

impl<R: Read + AsRawFd> Read for ControlledReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let deadline = self.control.instant()?;
        crate::process::read_exact_until(self.reader, bytes, deadline, || {
            self.control
                .cancellation
                .as_ref()
                .is_some_and(CompilerTransactionCancellation::is_cancelled)
        })
        .map_err(|error| {
            // Read::read_exact retries Interrupted. Cancellation is terminal.
            if error.kind() == io::ErrorKind::Interrupted {
                io::Error::new(io::ErrorKind::ConnectionAborted, error)
            } else {
                error
            }
        })?;
        self.control.checkpoint()?;
        Ok(bytes.len())
    }
}

pub(crate) fn monotonic_nanos() -> io::Result<u64> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one initialized timespec owned here.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) } != 0 {
        return Err(io::Error::last_os_error());
    }
    u64::try_from(value.tv_sec)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000_000_000))
        .and_then(|nanos| nanos.checked_add(value.tv_nsec as u64))
        .ok_or_else(|| invalid("monotonic deadline overflow"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MonotonicDeadline(u64);

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Exchange {
    nonce: [u8; 32],
    pub(crate) owner: NativeDemandOwner,
    deadline: MonotonicDeadline,
}

impl Exchange {
    pub(crate) fn new(owner: NativeDemandOwner) -> io::Result<Self> {
        let mut nonce = [0; 32];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut nonce)?;
        Ok(Self {
            nonce,
            owner,
            deadline: MonotonicDeadline(
                monotonic_nanos()?
                    .checked_add(
                        u64::try_from(crate::daemon::DEFAULT_REQUEST_DEADLINE.as_nanos())
                            .map_err(|_| invalid("native demand duration overflow"))?,
                    )
                    .ok_or_else(|| invalid("native demand deadline overflow"))?,
            ),
        })
    }

    pub(crate) fn control(
        &self,
        cancellation: Option<CompilerTransactionCancellation>,
    ) -> NativeDemandControl {
        NativeDemandControl {
            deadline: self.deadline,
            cancellation,
        }
    }

    pub(crate) fn limit_deadline(&mut self, duration: Duration) -> io::Result<()> {
        let duration = u64::try_from(duration.as_nanos())
            .map_err(|_| invalid("native demand duration overflow"))?;
        let deadline = monotonic_nanos()?
            .checked_add(duration)
            .ok_or_else(|| invalid("native demand deadline overflow"))?;
        self.deadline = self.deadline.min(MonotonicDeadline(deadline));
        self.control(None).checkpoint()
    }

    pub(crate) fn write(&self, writer: &mut (impl Write + ?Sized)) -> io::Result<()> {
        writer.write_all(MAGIC)?;
        writer.write_all(&self.nonce)?;
        writer.write_all(&self.owner.admission)?;
        writer.write_all(&self.owner.world)?;
        writer.write_all(&self.owner.inputs)?;
        writer.write_all(&self.deadline.0.to_le_bytes())
    }

    pub(crate) fn read(reader: &mut impl Read) -> io::Result<Self> {
        let mut magic = [0; 8];
        reader.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(invalid("native demand framing version mismatch"));
        }
        let mut nonce = [0; 32];
        reader.read_exact(&mut nonce)?;
        let mut admission = [0; 32];
        reader.read_exact(&mut admission)?;
        let mut world = [0; 32];
        reader.read_exact(&mut world)?;
        let mut inputs = [0; 32];
        reader.read_exact(&mut inputs)?;
        let mut deadline = [0; 8];
        reader.read_exact(&mut deadline)?;
        Ok(Self {
            nonce,
            owner: NativeDemandOwner {
                admission,
                world,
                inputs,
            },
            deadline: MonotonicDeadline(u64::from_le_bytes(deadline)),
        })
    }
}

pub(crate) struct ExchangeBudget {
    pub(crate) remaining: u32,
    sequence: u64,
    last_stage: Option<u64>,
}
impl ExchangeBudget {
    pub(crate) fn new() -> Self {
        Self {
            remaining: PAYLOAD_BUDGET,
            sequence: 0,
            last_stage: None,
        }
    }
    pub(crate) fn charge(&mut self, bytes: usize) -> io::Result<()> {
        let bytes = u32::try_from(bytes)
            .map_err(|_| invalid("native exchange payload exceeds operation budget"))?;
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or_else(|| invalid("native exchange payload exceeds operation budget"))?;
        Ok(())
    }
    pub(crate) fn frame(&mut self, reader: &mut impl Read) -> io::Result<Vec<u8>> {
        let mut length = [0; 4];
        reader.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        self.charge(length)?;
        let mut payload = vec![0; length];
        reader.read_exact(&mut payload)?;
        Ok(payload)
    }
    pub(crate) fn demand(&mut self, stage: u64, sequence: u64) -> io::Result<()> {
        if self.sequence.checked_add(1) != Some(sequence)
            || self.last_stage.is_some_and(|previous| stage < previous)
        {
            return Err(invalid("stale, duplicate or unordered native demand"));
        }
        self.sequence = sequence;
        self.last_stage = Some(stage);
        Ok(())
    }
}

pub(crate) fn read_coordinates(reader: &mut impl Read) -> io::Result<(u64, u64)> {
    let mut stage = [0; 8];
    reader.read_exact(&mut stage)?;
    let mut sequence = [0; 8];
    reader.read_exact(&mut sequence)?;
    Ok((u64::from_le_bytes(stage), u64::from_le_bytes(sequence)))
}

pub(crate) fn write_demand(
    writer: &mut impl Write,
    exchange: &Exchange,
    stage: u64,
    sequence: u64,
    payload: &[u8],
) -> io::Result<()> {
    writer.write_all(&[DEMAND])?;
    exchange.write(writer)?;
    writer.write_all(&stage.to_le_bytes())?;
    writer.write_all(&sequence.to_le_bytes())?;
    write_frame(writer, payload)?;
    writer.flush()
}

pub(crate) fn write_reply(
    writer: &mut impl Write,
    exchange: &Exchange,
    stage: u64,
    sequence: u64,
    reply: &NativeReply,
) -> io::Result<()> {
    writer.write_all(&[REPLY])?;
    exchange.write(writer)?;
    writer.write_all(&stage.to_le_bytes())?;
    writer.write_all(&sequence.to_le_bytes())?;
    let (tag, bytes) = match reply {
        NativeReply::Resolved(bytes) => (0, bytes),
        NativeReply::Refused(bytes) => (1, bytes),
    };
    writer.write_all(&[tag])?;
    write_frame(writer, bytes)?;
    writer.flush()
}

pub(crate) fn read_reply(
    reader: &mut impl Read,
    exchange: &Exchange,
    stage: u64,
    sequence: u64,
    budget: &mut ExchangeBudget,
) -> io::Result<NativeReply> {
    let mut tag = [0];
    reader.read_exact(&mut tag)?;
    if tag[0] != REPLY {
        return Err(invalid("native demand requires exactly one reply"));
    }
    let observed = Exchange::read(reader)?;
    if &observed != exchange || read_coordinates(reader)? != (stage, sequence) {
        return Err(invalid("native reply belongs to another demand"));
    }
    reader.read_exact(&mut tag)?;
    match tag[0] {
        0 => Ok(NativeReply::Resolved(budget.frame(reader)?)),
        1 => Ok(NativeReply::Refused(budget.frame(reader)?)),
        _ => Err(invalid("unknown native reply outcome")),
    }
}

fn write_frame(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .map_err(|_| invalid("native exchange frame exceeds format"))?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(payload)
}

/// One framing policy shared by client transports and the supervised relay.
/// Client permits only its first demand to tighten the deadline; Relay receives
/// an already-clamped header and requires exact equality on every demand.
pub(crate) struct Protocol {
    exchange: Exchange,
    pub(crate) budget: ExchangeBudget,
    role: DeadlineRole,
    phase: ProtocolPhase,
}

enum DeadlineRole {
    Client { first: bool },
    Relay,
}

#[derive(Clone, Copy)]
enum ProtocolPhase {
    Active,
    Refused,
    Terminal,
}

impl Protocol {
    pub(crate) fn client(exchange: Exchange) -> Self {
        Self::new(exchange, DeadlineRole::Client { first: true })
    }

    pub(crate) fn relay(exchange: Exchange) -> Self {
        Self::new(exchange, DeadlineRole::Relay)
    }

    fn new(exchange: Exchange, role: DeadlineRole) -> Self {
        Self {
            exchange,
            budget: ExchangeBudget::new(),
            role,
            phase: ProtocolPhase::Active,
        }
    }

    pub(crate) fn exchange(&self) -> &Exchange {
        &self.exchange
    }

    pub(crate) fn ensure_demand_allowed(&self) -> io::Result<()> {
        match self.phase {
            ProtocolPhase::Active => Ok(()),
            ProtocolPhase::Refused => Err(invalid("native demand after semantic refusal")),
            ProtocolPhase::Terminal => Err(invalid("native demand after terminal output")),
        }
    }

    pub(crate) fn demand(
        &mut self,
        observed: &Exchange,
        stage: u64,
        sequence: u64,
    ) -> io::Result<()> {
        self.ensure_demand_allowed()?;
        let first = matches!(self.role, DeadlineRole::Client { first: true });
        if self.exchange.nonce != observed.nonce
            || self.exchange.owner != observed.owner
            || if first {
                observed.deadline > self.exchange.deadline
            } else {
                observed.deadline != self.exchange.deadline
            }
        {
            return Err(invalid("native demand belongs to another active request"));
        }
        self.exchange.deadline = observed.deadline;
        if let DeadlineRole::Client { first } = &mut self.role {
            *first = false;
        }
        self.exchange.control(None).checkpoint()?;
        self.budget.demand(stage, sequence)?;
        self.budget.charge(ROUND_FRAME_BYTES)
    }

    pub(crate) fn reply(&mut self, reply: &NativeReply) {
        if matches!(self.phase, ProtocolPhase::Active) && matches!(reply, NativeReply::Refused(_)) {
            self.phase = ProtocolPhase::Refused;
        }
    }

    pub(crate) fn terminal(&mut self, success: bool) -> io::Result<()> {
        match self.phase {
            ProtocolPhase::Refused if success => {
                return Err(invalid("native refusal followed by successful output"));
            }
            ProtocolPhase::Terminal => return Err(invalid("duplicate native terminal output")),
            _ => {}
        }
        self.phase = ProtocolPhase::Terminal;
        Ok(())
    }

    pub(crate) fn resolve(
        &mut self,
        resolver: &mut dyn NativeDemandResolver,
        stage: u64,
        sequence: u64,
        payload: Vec<u8>,
        cancellation: Option<CompilerTransactionCancellation>,
    ) -> io::Result<NativeReply> {
        let control = self.exchange.control(cancellation);
        control.checkpoint()?;
        let reply = crate::endpoint::with_native_demand_control(control.clone(), || {
            resolver.resolve_native_demand(
                &NativeDemand {
                    owner: self.exchange.owner.clone(),
                    stage,
                    sequence,
                    payload,
                },
                &control,
            )
        })?;
        // Preserve the wire refusal before a later deadline or budget failure.
        self.reply(&reply);
        control.checkpoint()?;
        let bytes = match &reply {
            NativeReply::Resolved(bytes) | NativeReply::Refused(bytes) => bytes,
        };
        self.budget.charge(bytes.len())?;
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> NativeDemandOwner {
        NativeDemandOwner {
            admission: [1; 32],
            world: [2; 32],
            inputs: [3; 32],
        }
    }

    #[test]
    fn exact_native_reply_refuses_wrong_request_stage_sequence_and_outcome() {
        let exchange = Exchange::new(owner()).unwrap();
        let mut valid = Vec::new();
        write_reply(
            &mut valid,
            &exchange,
            7,
            1,
            &NativeReply::Refused(b"semantic".to_vec()),
        )
        .unwrap();
        assert!(
            matches!(read_reply(&mut valid.as_slice(), &exchange, 7, 1, &mut ExchangeBudget::new()).unwrap(), NativeReply::Refused(bytes) if bytes == b"semantic")
        );
        for offset in [0, 1, 9, 41, 73, 105, 137, 145, 153, 161] {
            let mut invalid = valid.clone();
            invalid[offset] ^= 0x80;
            assert!(
                read_reply(
                    &mut invalid.as_slice(),
                    &exchange,
                    7,
                    1,
                    &mut ExchangeBudget::new()
                )
                .is_err(),
                "offset {offset}"
            );
        }
        for end in [0, 1, 20, 144, 161, valid.len() - 1] {
            assert!(read_reply(
                &mut &valid[..end],
                &exchange,
                7,
                1,
                &mut ExchangeBudget::new()
            )
            .is_err());
        }
    }

    #[test]
    fn native_exchange_budget_refuses_cumulative_payload_before_body_read() {
        struct LengthOnly;
        impl Read for LengthOnly {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                assert_eq!(
                    bytes.len(),
                    4,
                    "oversized frame body must not be allocated or read"
                );
                bytes.copy_from_slice(&3u32.to_le_bytes());
                Ok(4)
            }
        }
        let mut budget = ExchangeBudget::new();
        budget.charge(PAYLOAD_BUDGET as usize - 2).unwrap();
        assert_eq!(
            budget.frame(&mut LengthOnly).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let mut budget = ExchangeBudget::new();
        budget.demand(2, 1).unwrap();
        assert!(budget.demand(2, 1).is_err());
        assert!(budget.demand(2, 3).is_err());
        assert!(budget.demand(1, 2).is_err());
        budget.demand(2, 2).unwrap();
    }

    #[test]
    fn native_exchange_tiny_deadline_and_server_tightening_do_not_refresh() {
        let original = Exchange::new(owner()).unwrap();
        let mut server = original.clone();
        server.limit_deadline(Duration::from_secs(1)).unwrap();
        assert!(server.deadline < original.deadline);
        let mut client = Protocol::client(original.clone());
        client.demand(&server, 0, 1).unwrap();
        assert_eq!(client.exchange().deadline, server.deadline);
        let mut changed = server.clone();
        changed.deadline.0 -= 1;
        assert!(client.demand(&changed, 0, 2).is_err());
        let other_request = Exchange::new(owner()).unwrap();
        assert!(Protocol::client(original.clone())
            .demand(&other_request, 0, 1)
            .is_err());
        // A relay has already received the server's clamped header. Even its
        // first worker demand cannot tighten or extend that deadline.
        assert!(Protocol::relay(original).demand(&server, 0, 1).is_err());
        let mut relay = Protocol::relay(server.clone());
        relay.demand(&server, 0, 1).unwrap();
        relay.demand(&server, 1, 2).unwrap();
        assert!(relay.demand(&changed, 1, 3).is_err());
        server.deadline = MonotonicDeadline(monotonic_nanos().unwrap());
        assert_eq!(
            server.control(None).checkpoint().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            server.limit_deadline(Duration::ZERO).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }
    #[test]
    fn native_protocol_refusal_survives_post_resolution_cancellation_and_budget_failure() {
        struct Refuse(Option<CompilerTransactionCancellation>);
        impl NativeDemandResolver for Refuse {
            fn resolve_native_demand(
                &mut self,
                _: &NativeDemand,
                _: &NativeDemandControl,
            ) -> io::Result<NativeReply> {
                if let Some(cancel) = &self.0 {
                    cancel.cancel();
                }
                Ok(NativeReply::Refused(b"first refusal".to_vec()))
            }
        }
        for cancelled in [false, true] {
            let exchange = Exchange::new(owner()).unwrap();
            let mut protocol = Protocol::client(exchange.clone());
            protocol.demand(&exchange, 0, 1).unwrap();
            let cancellation = cancelled.then(CompilerTransactionCancellation::new);
            if !cancelled {
                protocol.budget.remaining = 0;
            }
            assert!(protocol
                .resolve(
                    &mut Refuse(cancellation.clone()),
                    0,
                    1,
                    vec![],
                    cancellation
                )
                .is_err());
            assert!(protocol.demand(&exchange, 0, 2).is_err());
            assert!(protocol.terminal(true).is_err());
            protocol.terminal(false).unwrap();
        }
    }

    proptest::proptest! {
        #[test]
        fn native_protocol_refusal_cannot_be_cleared_by_later_replies(
            prefix in 0u64..8,
            replies in proptest::collection::vec(proptest::bool::ANY, 0..20),
        ) {
            for relay in [false, true] {
                let exchange = Exchange::new(owner()).unwrap();
                let mut protocol = if relay {
                    Protocol::relay(exchange.clone())
                } else {
                    Protocol::client(exchange.clone())
                };
                for sequence in 1..=prefix {
                    protocol.demand(&exchange, sequence, sequence).unwrap();
                    protocol.reply(&NativeReply::Resolved(vec![]));
                }
                protocol.demand(&exchange, prefix, prefix + 1).unwrap();
                protocol.reply(&NativeReply::Refused(vec![]));
                for refused in replies.iter().copied() {
                    let reply = if refused { NativeReply::Refused(vec![]) } else { NativeReply::Resolved(vec![]) };
                    protocol.reply(&reply);
                    let remaining = protocol.budget.remaining;
                    proptest::prop_assert!(protocol.demand(&exchange, prefix, prefix + 2).is_err());
                    proptest::prop_assert_eq!(protocol.budget.remaining, remaining);
                    proptest::prop_assert!(protocol.terminal(true).is_err());
                }
                proptest::prop_assert!(protocol.terminal(true).is_err());
                proptest::prop_assert!(protocol.terminal(false).is_ok());
                proptest::prop_assert!(protocol.terminal(false).is_err());
                proptest::prop_assert!(protocol.demand(&exchange, prefix, prefix + 2).is_err());
                // A new request receives fresh framing state.
                proptest::prop_assert!(Protocol::client(Exchange::new(owner()).unwrap()).terminal(true).is_ok());
            }
        }
    }
}
