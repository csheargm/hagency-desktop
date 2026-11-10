//! Cooperative control at cancel-safe leaf IO, never around a public read.
use super::*;
use std::{future::Future, pin::Pin};

pub(in crate::codex) enum Controlled<T> {
    Event(Event),
    Control(T),
}

/// The original encoded bytes move between this value and the sole writer.
/// Neither this value nor its bytes has a public reconstruction/clone path.
pub(in crate::codex) struct PreparedFrame {
    id: RequestId,
    bytes: Option<Vec<u8>>,
    deadline: Instant,
    write_deadline: Option<Instant>,
}

impl<R, W, E> Driver<R, W, E> {
    pub(in crate::codex) fn event_deadline(&self) -> Deadline {
        if self.shared_activity.is_some() {
            Deadline::UNBOUNDED
        } else {
            Deadline::after(Duration::from_millis(self.limits.event_wait_ms))
        }
    }
    pub(in crate::codex) fn control_policy_fits(&self, owner_ms: u64, reserve_ms: u64) -> bool {
        owner_ms != 0
            && reserve_ms >= self.limits.write_timeout_ms
            && owner_ms.checked_add(reserve_ms).is_some_and(|total| {
                total <= MAX_REQUEST_MS
                    && (if self.shared_activity.is_some() {
                        self.has_live_shared_activity()
                    } else {
                        Instant::now() + Duration::from_millis(total) <= self.lifetime
                    })
            })
    }
    pub(in crate::codex) fn approval_deadlines(
        &self,
        id: &RequestId,
        owner_ms: u64,
        reserve_ms: u64,
    ) -> Result<(Instant, Instant), Error> {
        let admitted = self
            .connection
            .approval_admitted_ms(id)
            .ok_or(Error::Closed)?;
        let owner = self.origin + Duration::from_millis(admitted + owner_ms);
        let response = owner + Duration::from_millis(reserve_ms);
        if owner <= Instant::now()
            || (if self.shared_activity.is_some() {
                !self.has_live_shared_activity()
            } else {
                response > self.lifetime
            })
        {
            return Err(Error::Timeout);
        }
        Ok((owner, response))
    }
    pub(in crate::codex) fn begin_prepared_send(
        &mut self,
        prepared: &mut PreparedFrame,
    ) -> Result<(), Error> {
        let deadline = *prepared.write_deadline.get_or_insert_with(|| {
            prepared
                .deadline
                .min(Instant::now() + Duration::from_millis(self.limits.write_timeout_ms))
        });
        self.check(deadline)
    }
    pub(in crate::codex) fn prepare_approval(
        &mut self,
        response: crate::codex::approval::ApprovalResponse,
        deadline: Instant,
    ) -> Result<PreparedFrame, Error> {
        self.check(deadline)?;
        let id = response.request.id().clone();
        let bytes = self.connection.respond_approval(response, self.now_ms())?;
        Ok(PreparedFrame {
            id,
            bytes: Some(bytes),
            deadline,
            write_deadline: None,
        })
    }
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, E: AsyncRead + Unpin> Driver<R, W, E> {
    pub(in crate::codex) async fn next_or_control<F: Future + ?Sized>(
        &mut self,
        mut control: Pin<&mut F>,
        deadline: Deadline,
    ) -> Result<Controlled<F::Output>, Error> {
        let operation = Operation {
            driver: self,
            finished: false,
        };
        let result = operation
            .driver
            .control_inner(control.as_mut(), deadline)
            .await;
        operation.finish(result)
    }

    async fn control_inner<F: Future + ?Sized>(
        &mut self,
        mut control: Pin<&mut F>,
        deadline: Deadline,
    ) -> Result<Controlled<F::Output>, Error> {
        loop {
            tokio::task::yield_now().await;
            self.check(deadline)?;
            if let Some(event) = self.buffered_event()? {
                return Ok(Controlled::Event(event));
            }
            let wake = self.next_deadline(deadline);
            let streams = self.streams.as_mut().ok_or(Error::Closed)?;
            // Only the borrowed control future and cancellation-safe single reads
            // are selected. Accepted bytes are stored before the next await.
            tokio::select! {
                biased;
                output = control.as_mut() => {
                    self.check(deadline)?;
                    return Ok(Controlled::Control(output));
                }
                _ = tokio::time::sleep_until(wake) => {
                    // A lease renewal can move the wake while this exact read
                    // remains pending. Recheck it without replacing the read or
                    // renewing the callback's original owner deadline.
                    self.check(deadline)?;
                    if self.next_deadline(deadline) <= Instant::now() {
                        return Err(Error::Timeout);
                    }
                    continue;
                },
                result = streams.stdout.read(&mut self.input) => {
                    self.input_end = result.map_err(|_| Error::Io("stdout read"))?;
                    self.input_start = 0;
                    if self.input_end == 0 { return Err(Error::PeerEof); }
                }
                result = streams.stderr.read(&mut self.stderr_buffer), if self.stderr_open => {
                    let n = result.map_err(|_| Error::Io("stderr read"))?;
                    if n == 0 { self.stderr_open = false; }
                    else { self.diagnostics.append(&self.stderr_buffer[..n])?; }
                }
            }
            self.check(deadline)?;
        }
    }

    pub(in crate::codex) async fn send_prepared_or_event(
        &mut self,
        prepared: &mut PreparedFrame,
    ) -> Result<Controlled<TransportWrite>, Error> {
        let operation = Operation {
            driver: self,
            finished: false,
        };
        let result = operation.driver.prepared_inner(prepared).await;
        operation.finish(result)
    }

    async fn prepared_inner(
        &mut self,
        prepared: &mut PreparedFrame,
    ) -> Result<Controlled<TransportWrite>, Error> {
        // This deadline is installed once, even if a received update returns
        // before the first byte. Continuation cannot restart the write clock.
        let deadline = *prepared.write_deadline.get_or_insert_with(|| {
            prepared
                .deadline
                .min(Instant::now() + Duration::from_millis(self.limits.write_timeout_ms))
        });
        self.check(deadline)?;
        if self.writing.is_some() {
            return Err(Error::Closed);
        }
        self.writing = Some(Writing {
            id: Some(prepared.id.clone()),
            bytes: prepared.bytes.take().ok_or(Error::Closed)?,
            offset: 0,
            flushed: false,
        });
        loop {
            tokio::task::yield_now().await;
            self.check(deadline)?;
            let writing = self.writing.as_ref().ok_or(Error::Closed)?;
            if writing.flushed {
                let writing = self.writing.take().ok_or(Error::Closed)?;
                return Ok(Controlled::Control(TransportWrite {
                    request_id: writing.id,
                    bytes: writing.bytes.len(),
                }));
            }
            if writing.offset != 0 {
                // Once any bytes are accepted, finish/flush the same frame or
                // fail with original accepted-byte evidence. Never retry it.
                self.step(deadline).await?;
                continue;
            }
            if let Some(event) = self.buffered_event()? {
                // Exact Vec custody returns to the original prepared value;
                // encoding, callback reservation and deadline are not repeated.
                prepared.bytes = Some(self.writing.take().ok_or(Error::Closed)?.bytes);
                return Ok(Controlled::Event(event));
            }
            if !self.connection.has_prepared_approval(&prepared.id) {
                return Err(Error::Closed);
            }
            self.first_write_step(deadline).await?;
        }
    }

    async fn first_write_step(&mut self, deadline: Instant) -> Result<(), Error> {
        let partial = self.has_partial_frame();
        let wake = self.next_deadline(deadline);
        let streams = self.streams.as_mut().ok_or(Error::Closed)?;
        let writing = self.writing.as_mut().ok_or(Error::Closed)?;
        // Read currently ready stdout before polling the first write. A partial
        // frame must finish and be delivered first. No keepalive is fabricated.
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(wake) => {
                self.check(deadline)?;
                if self.next_deadline(deadline) <= Instant::now() {
                    return Err(Error::Timeout);
                }
                return Ok(());
            },
            result = streams.stdout.read(&mut self.input) => {
                self.input_end = result.map_err(|_| Error::Io("stdout read"))?;
                self.input_start = 0;
                if self.input_end == 0 { return Err(Error::PeerEof); }
            }
            result = streams.stdin.write(&writing.bytes), if !partial => {
                let n = result.map_err(|_| Error::Io("stdin write"))?;
                if n == 0 { return Err(Error::PeerEof); }
                if n > writing.bytes.len() { return Err(Error::Io("stdin write")); }
                writing.offset = n;
            }
            result = streams.stderr.read(&mut self.stderr_buffer), if self.stderr_open => {
                let n = result.map_err(|_| Error::Io("stderr read"))?;
                if n == 0 { self.stderr_open = false; }
                else { self.diagnostics.append(&self.stderr_buffer[..n])?; }
            }
        }
        self.check(deadline)
    }
}

#[cfg(test)]
mod shared_wait_tests {
    use super::*;
    async fn ready() -> (
        Driver<tokio::io::DuplexStream, tokio::io::Sink, tokio::io::Empty>,
        tokio::io::DuplexStream,
    ) {
        let (stdout, mut peer) = tokio::io::duplex(4096);
        let mut driver = Driver::new(
            stdout,
            tokio::io::sink(),
            tokio::io::empty(),
            Limits {
                write_timeout_ms: 1000,
                event_wait_ms: 1000,
                lifetime_ms: 30_000,
            },
        )
        .unwrap();
        driver
            .send(Command::Initialize {
                client_version: "test".into(),
                response_timeout_ms: 1000,
            })
            .await
            .unwrap();
        peer.write_all(b"{\"id\":0,\"result\":{\"userAgent\":\"fixture/0.167.0\",\"platformFamily\":\"unix\",\"platformOs\":\"macos\",\"codexHome\":\"/fixture\"}}\n")
            .await
            .unwrap();
        assert!(matches!(
            driver.next_event().await,
            Ok(Event::Initialized { .. })
        ));
        driver.send(Command::Initialized).await.unwrap();
        (driver, peer)
    }

    #[tokio::test(start_paused = true)]
    async fn shared_silent_reads_survive_renewed_epochs_and_stop_when_lease_lapses() {
        for controlled in [false, true] {
            let (mut driver, mut peer) = ready().await;
            let clock = std::sync::Arc::new(std::sync::Mutex::new(Some(
                Instant::now() + Duration::from_secs(3),
            )));
            let current = clock.clone();
            driver
                .attach_shared_activity(std::sync::Arc::new(move || *current.lock().unwrap()))
                .unwrap();
            let renew = clock.clone();
            let renewer = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    *renew.lock().unwrap() = Some(Instant::now() + Duration::from_secs(3));
                }
            });
            let began = Instant::now();
            let read = async {
                if controlled {
                    let control = std::future::pending::<()>();
                    tokio::pin!(control);
                    let until = driver.event_deadline();
                    match driver.next_or_control(control.as_mut(), until).await? {
                        Controlled::Event(event) => Ok(event),
                        Controlled::Control(()) => panic!("not a host control event"),
                    }
                } else {
                    driver.next_event().await
                }
            };
            let (result, ()) = tokio::join!(read, async {
                tokio::time::sleep(Duration::from_secs(1801)).await;
                peer.write_all(b"{\"method\":\"fixture/notice\",\"params\":{}}\n")
                    .await
                    .unwrap();
            });
            assert!(matches!(result.unwrap(), Event::Notification { .. }));
            assert_eq!(began.elapsed(), Duration::from_secs(1801));
            renewer.abort();
            let expiry = clock.lock().unwrap().unwrap();
            assert_eq!(driver.next_event().await.err(), Some(Error::Timeout));
            assert_eq!(Instant::now(), expiry);
            *clock.lock().unwrap() = Some(Instant::now() + Duration::from_secs(3));
            assert_eq!(
                driver.next_event().await.err(),
                Some(Error::Closed),
                "a lapsed operation is never revived"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn shared_activity_does_not_unbound_rpc_or_partial_frames() {
        for partial in [false, true] {
            let (mut driver, mut peer) = ready().await;
            let lease = Instant::now() + Duration::from_secs(30);
            driver
                .attach_shared_activity(std::sync::Arc::new(move || Some(lease)))
                .unwrap();
            let began = Instant::now();
            if partial {
                peer.write_all(b"{").await.unwrap();
            } else {
                driver
                    .send(Command::Request {
                        method: "thread/read".into(),
                        params: serde_json::json!({"threadId":"t"}),
                        response_timeout_ms: 500,
                    })
                    .await
                    .unwrap();
            }
            assert_eq!(driver.next_event().await.err(), Some(Error::Timeout));
            assert_eq!(
                began.elapsed(),
                if partial {
                    Duration::from_secs(10)
                } else {
                    Duration::from_millis(500)
                }
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn shared_renewal_never_extends_the_original_control_deadline() {
        let (mut driver, _peer) = ready().await;
        let clock = std::sync::Arc::new(std::sync::Mutex::new(Some(
            Instant::now() + Duration::from_secs(3),
        )));
        let current = clock.clone();
        driver
            .attach_shared_activity(std::sync::Arc::new(move || *current.lock().unwrap()))
            .unwrap();
        let renewer = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                *clock.lock().unwrap() = Some(Instant::now() + Duration::from_secs(3));
            }
        });
        let began = Instant::now();
        let control = std::future::pending::<()>();
        tokio::pin!(control);
        assert!(matches!(
            driver
                .next_or_control(control.as_mut(), Deadline::after(Duration::from_secs(5)))
                .await,
            Err(Error::Timeout)
        ));
        assert_eq!(began.elapsed(), Duration::from_secs(5));
        renewer.abort();
    }

    #[test]
    fn human_wait_never_extends_or_revives_the_activity_lease() {
        let mut driver = Driver::new(
            (),
            (),
            (),
            Limits {
                write_timeout_ms: 1000,
                event_wait_ms: 30_000,
                lifetime_ms: 30_000,
            },
        )
        .unwrap();
        assert!(!driver.control_policy_fits(120_000, 5000));
        let live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let gate = live.clone();
        let deadline = Instant::now() + Duration::from_secs(20);
        driver
            .attach_shared_activity(std::sync::Arc::new(move || {
                gate.load(std::sync::atomic::Ordering::Acquire)
                    .then_some(deadline)
            }))
            .unwrap();
        assert!(driver.control_policy_fits(120_000, 5000));
        assert_eq!(driver.effective_lifetime(), deadline);
        assert!(!driver.control_policy_fits(MAX_REQUEST_MS, 5000));
        live.store(false, std::sync::atomic::Ordering::Release);
        assert!(!driver.control_policy_fits(1000, 1000));
        assert!(
            driver
                .check(Instant::now() + Duration::from_secs(120))
                .is_err()
        );
    }
}
