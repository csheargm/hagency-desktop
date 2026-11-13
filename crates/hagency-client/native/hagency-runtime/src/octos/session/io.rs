//! The OUP stdio wire (ADR-193): the Claude wire's bounded line transport,
//! byte-exact write progress and prepared frames, on Octos frames.
use super::Error;
use crate::octos::{Decoder, Frame};
use std::{collections::VecDeque, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::Instant,
};

const READ_BYTES: usize = 16 * 1024;
const MAX_QUEUED: usize = 16;
const MAX_QUEUED_BYTES: usize = 2 * 1024 * 1024;
const MAX_DURATION_MS: u64 = 1_200_000;
const STDERR_BYTES: usize = 16 * 1024;
// A revoked lease has no independent wakeup channel. Recheck it even while
// the peer is silent, rather than waiting until its previous expiration.
const ACTIVITY_RECHECK_MS: u64 = 100;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub write_timeout_ms: u64,
    pub event_wait_ms: u64,
    pub lifetime_ms: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            write_timeout_ms: 10_000,
            event_wait_ms: 60_000,
            lifetime_ms: MAX_DURATION_MS,
        }
    }
}
impl Limits {
    pub(crate) fn validate(self) -> Result<(), Error> {
        if [self.write_timeout_ms, self.event_wait_ms, self.lifetime_ms]
            .iter()
            .any(|&value| value == 0 || value > MAX_DURATION_MS)
        {
            return Err(Error::Configuration);
        }
        Ok(())
    }
}
/// Diagnostic byte counts, never replay permission or peer acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteProgress {
    pub accepted_bytes: usize,
    pub total_bytes: usize,
    pub flushed: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Termination {
    pub cause: Error,
    pub unconfirmed_write: Option<WriteProgress>,
}
/// Deliberately no Debug/Serialize; stderr may contain private runtime text.
pub struct StderrSnapshot {
    pub total_bytes: u64,
    pub tail: Vec<u8>,
}
struct Streams<R, W, E> {
    stdout: R,
    stdin: W,
    stderr: E,
}
struct Writing {
    bytes: Vec<u8>,
    offset: usize,
    flushed: bool,
    prepared: Option<u64>,
}
pub(super) struct Received {
    pub message: Frame,
    pub at: Instant,
}
pub(super) enum Controlled<T> {
    Message(Received),
    Control(T),
}
pub(super) struct PreparedFrame {
    id: u64,
    bytes: Option<Vec<u8>>,
    deadline: Instant,
    write_deadline: Option<Instant>,
}
impl PreparedFrame {
    pub fn new(id: u64, bytes: Vec<u8>, deadline: Instant) -> Self {
        Self {
            id,
            bytes: Some(bytes),
            deadline,
            write_deadline: None,
        }
    }
}
pub(super) struct Wire<R, W, E> {
    streams: Option<Streams<R, W, E>>,
    decoder: Decoder,
    limits: Limits,
    origin: Instant,
    lifetime: Instant,
    activity: Option<Arc<dyn Fn() -> Option<Instant> + Send + Sync>>,
    input: [u8; READ_BYTES],
    start: usize,
    end: usize,
    input_at: Instant,
    stderr_buffer: [u8; READ_BYTES],
    stderr_open: bool,
    stderr_tail: VecDeque<u8>,
    stderr_total: u64,
    queue: VecDeque<(Received, usize)>,
    queued_bytes: usize,
    writing: Option<Writing>,
    termination: Option<Termination>,
}
impl<R, W, E> Wire<R, W, E> {
    pub fn new(stdout: R, stdin: W, stderr: E, limits: Limits) -> Result<Self, Error> {
        limits.validate()?;
        let origin = Instant::now();
        Ok(Self {
            streams: Some(Streams {
                stdout,
                stdin,
                stderr,
            }),
            decoder: Decoder::default(),
            limits,
            origin,
            lifetime: origin + Duration::from_millis(limits.lifetime_ms),
            activity: None,
            input: [0; READ_BYTES],
            start: 0,
            end: 0,
            input_at: origin,
            stderr_buffer: [0; READ_BYTES],
            stderr_open: true,
            stderr_tail: VecDeque::new(),
            stderr_total: 0,
            queue: VecDeque::new(),
            queued_bytes: 0,
            writing: None,
            termination: None,
        })
    }
    pub fn event_deadline(&self) -> Instant {
        Instant::now() + Duration::from_millis(self.limits.event_wait_ms)
    }
    /// Adds a restriction to this wire. It cannot create a lease or extend
    /// the lifetime of an operation, and can only be installed once.
    pub fn restrict_activity(
        &mut self,
        activity: Arc<dyn Fn() -> Option<Instant> + Send + Sync>,
    ) -> Result<(), Error> {
        if self.activity.is_some()
            || self.termination.is_some()
            || activity().is_none_or(|until| until <= Instant::now())
        {
            return Err(Error::State);
        }
        self.activity = Some(activity);
        Ok(())
    }
    /// The fixed end of this wire's life; no wait outlasts it.
    pub fn lifetime(&self) -> Instant {
        self.lifetime
    }
    pub fn permission_deadlines(
        &self,
        at: Instant,
        owner_ms: u64,
        reserve_ms: u64,
    ) -> Result<(Instant, Instant), Error> {
        if owner_ms == 0
            || reserve_ms < self.limits.write_timeout_ms
            || owner_ms
                .checked_add(reserve_ms)
                .is_none_or(|ms| ms > MAX_DURATION_MS)
        {
            return Err(Error::Configuration);
        }
        let owner = at + Duration::from_millis(owner_ms);
        let response = owner + Duration::from_millis(reserve_ms);
        if owner <= Instant::now() || response > self.lifetime {
            return Err(Error::Timeout);
        }
        Ok((owner, response))
    }
    pub fn termination(&self) -> Option<&Termination> {
        self.termination.as_ref()
    }
    /// The prepared frame in write custody, by its sequence, if any.
    pub fn writing_prepared(&self) -> Option<u64> {
        self.writing.as_ref().and_then(|w| w.prepared)
    }
    pub fn write_progress(&self) -> Option<WriteProgress> {
        self.writing.as_ref().map(|w| WriteProgress {
            accepted_bytes: w.offset,
            total_bytes: w.bytes.len(),
            flushed: w.flushed,
        })
    }
    pub fn stderr_snapshot(&self) -> StderrSnapshot {
        StderrSnapshot {
            total_bytes: self.stderr_total,
            tail: self.stderr_tail.iter().copied().collect(),
        }
    }
    pub fn close(&mut self, cause: Error) {
        if self.termination.is_some() {
            return;
        }
        self.termination = Some(Termination {
            cause,
            unconfirmed_write: self.write_progress(),
        });
        self.streams = None;
        self.writing = None;
        self.queue.clear();
        self.queued_bytes = 0;
        self.start = 0;
        self.end = 0;
        let _ = self.decoder.eof();
    }
    fn now_ms(&self) -> u64 {
        Instant::now()
            .duration_since(self.origin)
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
    pub fn check(&mut self, until: Instant) -> Result<(), Error> {
        if self.termination.is_some() {
            return Err(Error::Closed);
        }
        self.decoder.check_deadline(self.now_ms())?;
        let now = Instant::now();
        if now >= until.min(self.lifetime)
            || self
                .activity
                .as_ref()
                .is_some_and(|activity| activity().is_none_or(|deadline| now >= deadline))
        {
            return Err(Error::Timeout);
        }
        Ok(())
    }
    fn deadline(&self, until: Instant) -> Instant {
        let partial = self
            .decoder
            .deadline_ms()
            .and_then(|ms| self.origin.checked_add(Duration::from_millis(ms)))
            .unwrap_or(self.lifetime);
        let mut deadline = until.min(self.lifetime).min(partial);
        if let Some(activity) = &self.activity {
            let now = Instant::now();
            deadline = deadline
                .min(activity().unwrap_or(now))
                .min(now + Duration::from_millis(ACTIVITY_RECHECK_MS));
        }
        deadline
    }
    fn parse(&mut self) -> Result<(), Error> {
        let before = self.decoder.buffered_bytes();
        let (consumed, message) = self
            .decoder
            .feed(&self.input[self.start..self.end], self.now_ms())?;
        self.start += consumed;
        if let Some(message) = message {
            let charge = before.checked_add(consumed).ok_or(Error::Capacity)?;
            let total = self
                .queued_bytes
                .checked_add(charge)
                .ok_or(Error::Capacity)?;
            if self.queue.len() >= MAX_QUEUED || total > MAX_QUEUED_BYTES {
                return Err(Error::Capacity);
            }
            self.queue.push_back((
                Received {
                    message,
                    at: self.input_at,
                },
                charge,
            ));
            self.queued_bytes = total;
        }
        Ok(())
    }
    fn buffered(&mut self) -> Result<Option<Received>, Error> {
        // At most one decoded message per parse. Never await between accepting
        // read bytes and storing their offset or timestamp.
        while self.queue.is_empty() && self.start < self.end {
            self.parse()?;
        }
        if let Some((received, charge)) = self.queue.pop_front() {
            self.queued_bytes -= charge;
            Ok(Some(received))
        } else {
            Ok(None)
        }
    }
    fn stderr(&mut self, n: usize) -> Result<(), Error> {
        self.stderr_total = self
            .stderr_total
            .checked_add(n as u64)
            .ok_or(Error::Capacity)?;
        let discard = (self.stderr_tail.len() + n).saturating_sub(STDERR_BYTES);
        self.stderr_tail
            .drain(..discard.min(self.stderr_tail.len()));
        self.stderr_tail.extend(
            self.stderr_buffer[..n]
                .iter()
                .skip(n.saturating_sub(STDERR_BYTES))
                .copied(),
        );
        Ok(())
    }
}
impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, E: AsyncRead + Unpin> Wire<R, W, E> {
    pub async fn send(&mut self, bytes: Vec<u8>, until: Instant) -> Result<WriteProgress, Error> {
        let until = until.min(Instant::now() + Duration::from_millis(self.limits.write_timeout_ms));
        self.check(until)?;
        if self.writing.is_some() {
            return Err(Error::State);
        }
        self.writing = Some(Writing {
            bytes,
            offset: 0,
            flushed: false,
            prepared: None,
        });
        loop {
            tokio::task::yield_now().await;
            self.check(until)?;
            if let Some(progress) = self.write_progress().filter(|value| value.flushed) {
                self.writing = None;
                return Ok(progress);
            }
            self.step(until, true, false).await?;
        }
    }
    pub async fn next(&mut self, until: Instant) -> Result<Received, Error> {
        loop {
            tokio::task::yield_now().await;
            self.check(until)?;
            if let Some(received) = self.buffered()? {
                return Ok(received);
            }
            self.step(until, false, false).await?;
        }
    }
    async fn step(&mut self, until: Instant, write: bool, barrier: bool) -> Result<(), Error> {
        let control = std::future::pending::<()>();
        tokio::pin!(control);
        self.step_control(until, write, barrier, control.as_mut())
            .await?;
        Ok(())
    }
    async fn step_control<F: std::future::Future + ?Sized>(
        &mut self,
        until: Instant,
        write: bool,
        barrier: bool,
        mut control: std::pin::Pin<&mut F>,
    ) -> Result<Option<F::Output>, Error> {
        self.check(until)?;
        if self.start < self.end {
            self.parse()?;
            return Ok(None);
        }
        let deadline = self.deadline(until);
        let partial = barrier && self.decoder.buffered_bytes() != 0;
        let streams = self.streams.as_mut().ok_or(Error::Closed)?;
        enum Observed<T> {
            Write(std::io::Result<usize>),
            Flush(std::io::Result<()>),
            Read(std::io::Result<usize>),
            Stderr(std::io::Result<usize>),
            Deadline,
            Control(T),
        }
        let writing = write
            && !partial
            && self
                .writing
                .as_ref()
                .is_some_and(|w| w.offset < w.bytes.len());
        let flushing = write
            && !partial
            && self
                .writing
                .as_ref()
                .is_some_and(|w| w.offset == w.bytes.len() && !w.flushed);
        let observed = tokio::select! {
            biased;
            _=tokio::time::sleep_until(deadline)=>Observed::Deadline,
            result=streams.stdout.read(&mut self.input)=>Observed::Read(result),
            output=control.as_mut()=>Observed::Control(output),
            result=async {
                if writing {
                    let frame=self.writing.as_ref().ok_or(Error::Closed)?;
                    Ok::<_,Error>(Observed::Write(streams.stdin.write(&frame.bytes[frame.offset..]).await))
                } else {Ok(Observed::Flush(streams.stdin.flush().await))}
            }, if writing || flushing=>result?,
            result=streams.stderr.read(&mut self.stderr_buffer), if self.stderr_open=>Observed::Stderr(result),
        };
        match observed {
            Observed::Control(output) => {
                self.check(until)?;
                return Ok(Some(output));
            }
            Observed::Deadline => {
                // A live lease can renew while this very read is waiting.
                // Preserve the in-flight read/write and evaluate it again.
                self.check(until)?;
                return Ok(None);
            }
            Observed::Write(Ok(0)) => return Err(Error::Io("stdin write zero")),
            Observed::Read(Ok(0)) => {
                self.decoder.eof()?;
                return Err(Error::PeerEof);
            }
            Observed::Write(Ok(n)) => {
                let frame = self.writing.as_mut().ok_or(Error::Closed)?;
                frame.offset = frame
                    .offset
                    .checked_add(n)
                    .filter(|&n| n <= frame.bytes.len())
                    .ok_or(Error::Io("stdin write"))?;
            }
            Observed::Flush(Ok(())) => self.writing.as_mut().ok_or(Error::Closed)?.flushed = true,
            Observed::Read(Ok(n)) => {
                self.start = 0;
                self.end = n;
                self.input_at = Instant::now();
            }
            Observed::Stderr(Ok(0)) => self.stderr_open = false,
            Observed::Stderr(Ok(n)) => self.stderr(n)?,
            Observed::Write(Err(_)) => return Err(Error::Io("stdin write")),
            Observed::Flush(Err(_)) => return Err(Error::Io("stdin flush")),
            Observed::Read(Err(_)) => return Err(Error::Io("stdout read")),
            Observed::Stderr(Err(_)) => return Err(Error::Io("stderr read")),
        }
        self.check(until)?;
        Ok(None)
    }

    pub async fn next_or_control<F: std::future::Future + ?Sized>(
        &mut self,
        mut control: std::pin::Pin<&mut F>,
        until: Instant,
    ) -> Result<Controlled<F::Output>, Error> {
        loop {
            tokio::task::yield_now().await;
            self.check(until)?;
            if let Some(received) = self.buffered()? {
                return Ok(Controlled::Message(received));
            }
            if let Some(output) = self
                .step_control(until, false, false, control.as_mut())
                .await?
            {
                return Ok(Controlled::Control(output));
            }
        }
    }
    pub async fn send_prepared(
        &mut self,
        frame: &mut PreparedFrame,
        control_deadline: Instant,
    ) -> Result<Controlled<WriteProgress>, Error> {
        let until = (*frame.write_deadline.get_or_insert_with(|| {
            frame
                .deadline
                .min(Instant::now() + Duration::from_millis(self.limits.write_timeout_ms))
        }))
        .min(control_deadline);
        self.check(until)?;
        match &self.writing {
            Some(writing) if writing.prepared == Some(frame.id) && frame.bytes.is_none() => {}
            Some(_) => return Err(Error::State),
            None => {
                self.writing = Some(Writing {
                    bytes: frame.bytes.take().ok_or(Error::PermissionUnavailable)?,
                    offset: 0,
                    flushed: false,
                    prepared: Some(frame.id),
                })
            }
        }
        loop {
            tokio::task::yield_now().await;
            self.check(until)?;
            if let Some(progress) = self.write_progress().filter(|value| value.flushed) {
                self.writing = None;
                return Ok(Controlled::Control(progress));
            }
            // Original bytes/offset stay in the sole wire owner across each
            // event return. Read/control calls cannot advance that writer.
            if let Some(received) = self.buffered()? {
                return Ok(Controlled::Message(received));
            }
            self.step(until, true, true).await?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[tokio::test(start_paused = true)]
    async fn renewed_activity_keeps_the_original_pending_read() {
        let (stdout, mut peer) = tokio::io::duplex(1024);
        let mut wire = Wire::new(stdout, tokio::io::sink(), tokio::io::empty(), Limits::default()).unwrap();
        let started = Instant::now();
        let lease = Arc::new(Mutex::new(Some(started + Duration::from_millis(250))));
        let observed = lease.clone();
        wire.restrict_activity(Arc::new(move || *observed.lock().unwrap())).unwrap();
        assert_eq!(wire.restrict_activity(Arc::new(|| Some(Instant::now() + Duration::from_secs(1)))), Err(Error::State));
        let updater = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            *lease.lock().unwrap() = Some(Instant::now() + Duration::from_millis(600));
            tokio::time::sleep(Duration::from_millis(250)).await;
            peer.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"reply\",\"result\":{}}\n").await.unwrap();
        });
        let received = wire.next(started + Duration::from_secs(2)).await.unwrap();
        assert!(matches!(received.message, Frame::Response { ref id, .. } if id == "reply"));
        assert!(Instant::now() >= started + Duration::from_millis(350));
        updater.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn revoked_activity_interrupts_a_silent_peer() {
        let (stdout, _peer) = tokio::io::duplex(1024);
        let mut wire = Wire::new(stdout, tokio::io::sink(), tokio::io::empty(), Limits::default()).unwrap();
        let started = Instant::now();
        let lease = Arc::new(Mutex::new(Some(started + Duration::from_secs(5))));
        let observed = lease.clone();
        wire.restrict_activity(Arc::new(move || *observed.lock().unwrap())).unwrap();
        let updater = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            *lease.lock().unwrap() = None;
        });
        assert!(matches!(wire.next(started + Duration::from_secs(10)).await, Err(Error::Timeout)));
        assert!(Instant::now() <= started + Duration::from_millis(300));
        updater.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn expired_activity_is_not_installed() {
        let (stdout, _peer) = tokio::io::duplex(1024);
        let mut wire = Wire::new(stdout, tokio::io::sink(), tokio::io::empty(), Limits::default()).unwrap();
        assert_eq!(wire.restrict_activity(Arc::new(|| Some(Instant::now()))), Err(Error::State));
        assert!(wire.restrict_activity(Arc::new(|| Some(Instant::now() + Duration::from_secs(1)))).is_ok());
    }
}
