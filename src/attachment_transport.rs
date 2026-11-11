//! Bounded ordinary attachments. Network waits and blocking filesystem IO belong
//! to four original workers; dropping an async observer only requests cancellation.
mod files;
pub(crate) mod io_worker;
use crate::transfer_budget::{ByteBudget, Reservation};
use matrix_sdk::{
    Client,
    ruma::{
        api::{Metadata, OutgoingRequestExt, auth_scheme::SendAccessToken},
        events::room::MediaSource,
    },
};
use std::{
    collections::VecDeque,
    fs::File,
    future::Future,
    io::{self, Read, Write},
    path::Path,
    sync::{
        Arc, Condvar, LazyLock, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

// File downloads are disk-backed; only the shared temporary-storage budget limits them.
pub const MAX_FILE_BYTES: u64 = u64::MAX;
const MAX_DISK_BYTES: u64 = crate::transfer_budget::MAX_TEMPORARY_BYTES;
const IO_BUFFER: usize = 64 * 1024;
pub(crate) const THEME_LIMIT: u64 = 256 * 1024;
pub(crate) const MARKDOWN_LIMIT: u64 = 512 * 1024;
const TTL: Duration = Duration::from_secs(30 * 60);
type Current = Arc<dyn Fn() -> bool + Send + Sync>;

struct Stored {
    file: files::Scratch,
    quota: Option<Reservation>,
}
struct Record {
    stored: Mutex<Stored>,
    pipeline: Mutex<Option<Arc<Mutex<Reservation>>>>,
    len: AtomicU64,
    verified: AtomicBool,
    active_io: AtomicUsize,
    current: Current,
    stop: Arc<AtomicBool>,
    // In-flight deadline is fixed; verified plaintext receives one fresh TTL.
    expires: Instant,
    ready_expires: OnceLock<Instant>,
}
struct Writer {
    file: File,
    record: Arc<Record>,
}
impl Drop for Writer {
    fn drop(&mut self) {
        self.record.active_io.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Reader {
    file: File,
    record: Arc<Record>,
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.record.active_io.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.file.read(bytes)
    }
}
impl Record {
    fn expires_at(&self) -> Instant {
        self.ready_expires.get().copied().unwrap_or(self.expires)
    }
    fn finalize_at(self: &Arc<Self>, completed: Instant) -> Result<DownloadedFile, String> {
        // Serialize the publication/expiry change with cleanup, without doing IO
        // under a time lock. This transition is single-use, after verified EOF.
        let stored = self.stored.lock().unwrap();
        if self.stop.load(Ordering::Acquire)
            || !(self.current)()
            || stored.quota.is_none()
            || completed >= self.expires
        {
            return Err("Attachment transfer expired or account retired before completion".into());
        }
        self.ready_expires
            .set(completed + TTL)
            .map_err(|_| "Attachment was already finalized".to_owned())?;
        self.verified.store(true, Ordering::Release);
        Ok(DownloadedFile(self.clone()))
    }
    fn open(self: &Arc<Self>) -> io::Result<Reader> {
        let stored = self.stored.lock().unwrap();
        if self.stop.load(Ordering::Acquire)
            || stored.quota.is_none()
            || Instant::now() >= self.expires_at()
            || !(self.current)()
        {
            return Err(io::Error::other(
                "Attachment account retired or file expired",
            ));
        }
        let file = stored.file.open()?;
        self.active_io.fetch_add(1, Ordering::AcqRel);
        Ok(Reader {
            file,
            record: self.clone(),
        })
    }
}
/// Only a complete, integrity-verified plaintext file is published.
#[derive(Clone)]
pub struct DownloadedFile(Arc<Record>);
impl DownloadedFile {
    pub fn len(&self) -> u64 {
        self.0.len.load(Ordering::Acquire)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub(crate) fn is_current(&self) -> bool {
        self.is_current_at(Instant::now())
    }
    fn is_current_at(&self, now: Instant) -> bool {
        !self.0.stop.load(Ordering::Acquire)
            && self.0.verified.load(Ordering::Acquire)
            && now < self.0.expires_at()
            && (self.0.current)()
    }
    /// Synchronous bounded IO; caller must use an owned blocking worker.
    pub fn read_to(&self, target: &mut impl Write) -> io::Result<u64> {
        if !self.is_current() {
            return Err(io::Error::other(
                "Attachment account retired or file expired",
            ));
        }
        let mut source = self.0.open()?;
        let mut block = [0u8; IO_BUFFER];
        let mut total = 0;
        loop {
            if !self.is_current() {
                return Err(io::Error::other(
                    "Attachment account retired or file expired",
                ));
            }
            let count = source.read(&mut block)?;
            if count == 0 {
                break;
            }
            total += count as u64;
            if total > self.len() {
                return Err(io::Error::other("Attachment file grew"));
            }
            target.write_all(&block[..count])?;
        }
        if total != self.len() {
            return Err(io::Error::other("Attachment file truncated"));
        }
        Ok(total)
    }
    pub(crate) fn read_small(&self, limit: u64) -> Result<Vec<u8>, String> {
        if self.len() > limit {
            return Err("Attachment preview exceeds its size limit".into());
        }
        let mut output = Vec::with_capacity(self.len() as usize);
        self.read_to(&mut output)
            .map_err(|error| error.to_string())?;
        Ok(output)
    }
    pub(crate) fn save_to(&self, path: &Path) -> Result<(), String> {
        self.save_with(service(), path)
    }
    fn save_with(&self, service: &Service, path: &Path) -> Result<(), String> {
        let mut records = service.records.lock().unwrap();
        if records.len() >= 64 || !self.is_current() {
            return Err("Attachment account retired or file operations are busy".into());
        }
        let root = service
            .root
            .get_or_init(|| {
                files::Root::new_with_owner(&service.cache, service.creates.clone())
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(Clone::clone)?;
        let (scratch, file) = root.destination(path).map_err(|error| error.to_string())?;
        let record = Arc::new(Record {
            pipeline: Mutex::new(None),
            stored: Mutex::new(Stored {
                file: scratch,
                quota: Some(service.disk.reserve(0, MAX_DISK_BYTES)?),
            }),
            len: AtomicU64::new(0),
            verified: AtomicBool::new(false),
            active_io: AtomicUsize::new(1),
            current: self.0.current.clone(),
            stop: service.stopped.clone(),
            expires: self.0.expires_at(),
            ready_expires: OnceLock::new(),
        });
        records.push(record.clone());
        drop(records);
        let mut target = Writer {
            file,
            record: record.clone(),
        };
        struct Target<'a> {
            service: &'a Service,
            record: &'a Record,
            writer: &'a mut Writer,
        }
        impl Write for Target<'_> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.service
                    .append(self.record, self.writer, bytes, MAX_FILE_BYTES)
                    .map_err(io::Error::other)?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                self.writer.file.flush()
            }
        }
        self.read_to(&mut Target {
            service,
            record: &record,
            writer: &mut target,
        })
        .map_err(|error| error.to_string())?;
        target.file.sync_all().map_err(|error| error.to_string())?;
        // Account retirement serializes against the single final publication.
        // Copying/fsync happened on the owned worker outside this short fence.
        let _session = crate::persistence::SESSION_IO.lock().unwrap();
        if !self.is_current() {
            return Err("Attachment account retired before saving".into());
        }
        record
            .stored
            .lock()
            .unwrap()
            .file
            .publish(path)
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}
struct Job {
    epoch: u64,
    client: Client,
    source: MediaSource,
    limit: u64,
    current: Current,
    live: Arc<AtomicBool>,
    done: oneshot::Sender<Result<DownloadedFile, String>>,
}
struct Service {
    queue: Mutex<VecDeque<Job>>,
    wake: Condvar,
    root: OnceLock<Result<Arc<files::Root>, String>>,
    creates: Arc<files::CreateOwner>,
    cache: std::path::PathBuf,
    disk: Arc<ByteBudget>,
    pipelines: Arc<ByteBudget>,
    records: Mutex<Vec<Arc<Record>>>,
    active: AtomicUsize,
    backing_off: AtomicUsize,
    cleanup_failures: AtomicUsize,
    #[cfg(test)]
    last_error: Mutex<Option<String>>,
    stopped: Arc<AtomicBool>,
    joins: Mutex<Vec<std::thread::JoinHandle<()>>>,
}
impl Service {
    fn start(cache: std::path::PathBuf) -> Arc<Self> {
        Self::start_budget(cache, crate::transfer_budget::temporary_budget())
    }
    fn start_budget(cache: std::path::PathBuf, disk: Arc<ByteBudget>) -> Arc<Self> {
        let service = Arc::new(Self {
            queue: Mutex::default(),
            wake: Condvar::new(),
            root: OnceLock::new(),
            creates: Arc::default(),
            cache,
            disk,
            pipelines: Arc::default(),
            records: Mutex::default(),
            active: AtomicUsize::new(0),
            backing_off: AtomicUsize::new(0),
            cleanup_failures: AtomicUsize::new(0),
            #[cfg(test)]
            last_error: Mutex::default(),
            stopped: Arc::new(AtomicBool::new(false)),
            joins: Mutex::default(),
        });
        for index in 0..4 {
            let worker = service.clone();
            let join = std::thread::Builder::new()
                .name(format!("attachment-download-{index}"))
                .spawn(move || loop {
                    let job = {
                        let mut queue = worker.queue.lock().unwrap();
                        while queue.is_empty() && !worker.stopped.load(Ordering::Acquire) {
                            queue = worker.wake.wait(queue).unwrap();
                        }
                        if worker.stopped.load(Ordering::Acquire) {
                            break;
                        }
                        let job = queue.pop_front().unwrap();
                        worker.active.fetch_add(1, Ordering::AcqRel);
                        job
                    };
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        worker.perform(&job)
                    }))
                    .unwrap_or_else(|_| {
                        Err("Attachment worker failed; cleanup remains observed".into())
                    });
                    #[cfg(test)]
                    if let Err(error) = &result {
                        *worker.last_error.lock().unwrap() = Some(error.clone());
                    }
                    let _ = job.done.send(result);
                    worker.active.fetch_sub(1, Ordering::AcqRel);
                    worker.wake.notify_all();
                })
                .expect("bounded attachment worker");
            service.joins.lock().unwrap().push(join);
        }
        let cleanup = service.clone();
        let join = std::thread::Builder::new()
            .name("attachment-cleanup".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(100));
                cleanup.clean();
                if cleanup.stopped.load(Ordering::Acquire)
                    && cleanup.active.load(Ordering::Acquire) == 0
                    && cleanup.records.lock().unwrap().is_empty()
                    && cleanup.creates.failures() == 0
                {
                    cleanup.wake.notify_all();
                    break;
                }
            })
            .expect("attachment cleanup worker");
        service.joins.lock().unwrap().push(join);
        service
    }
    #[cfg(test)]
    fn new_file(&self, current: Current) -> Result<(Arc<Record>, Writer), String> {
        self.new_file_until(current, Instant::now() + TTL)
    }
    fn new_file_until(
        &self,
        current: Current,
        expires: Instant,
    ) -> Result<(Arc<Record>, Writer), String> {
        let mut records = self.records.lock().unwrap();
        if records.len() >= 64 {
            return Err("Too many attachment files are still in use".into());
        }
        let root = self
            .root
            .get_or_init(|| {
                files::Root::new_with_owner(&self.cache, self.creates.clone())
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(Clone::clone)?;
        let (file, writer) = root.create().map_err(|error| error.to_string())?;
        let record = Arc::new(Record {
            pipeline: Mutex::new(None),
            stored: Mutex::new(Stored {
                file,
                quota: Some(self.disk.reserve(0, MAX_DISK_BYTES)?),
            }),
            len: AtomicU64::new(0),
            verified: AtomicBool::new(false),
            active_io: AtomicUsize::new(1),
            current,
            stop: self.stopped.clone(),
            expires,
            ready_expires: OnceLock::new(),
        });
        records.push(record.clone());
        Ok((
            record.clone(),
            Writer {
                file: writer,
                record,
            },
        ))
    }
    fn append(
        &self,
        record: &Record,
        writer: &mut Writer,
        bytes: &[u8],
        limit: u64,
    ) -> Result<(), String> {
        if self.creates.failures() != 0 {
            return Err("Private file creation cleanup remains unconfirmed".into());
        }
        let len = record.len.load(Ordering::Acquire);
        if bytes.len() as u64 > limit.saturating_sub(len) {
            return Err("Attachment exceeds its size limit".into());
        }
        record
            .stored
            .lock()
            .unwrap()
            .quota
            .as_mut()
            .ok_or("Attachment file was retired")?
            .grow(bytes.len() as u64, MAX_DISK_BYTES)?;
        writer
            .file
            .write_all(bytes)
            .map_err(|error| error.to_string())?;
        record.len.fetch_add(bytes.len() as u64, Ordering::Release);
        Ok(())
    }
    fn clean(&self) {
        self.creates.retry();
        let mut failures = 0;
        self.records.lock().unwrap().retain(|record| {
            let mut stored = record.stored.lock().unwrap();
            let retired = self.stopped.load(Ordering::Acquire)
                || Instant::now() >= record.expires_at()
                || !(record.current)();
            if record.active_io.load(Ordering::Acquire) != 0
                || (!retired && Arc::strong_count(record) != 1)
            {
                return true;
            }
            record.verified.store(false, Ordering::Release);
            match stored.file.remove() {
                Ok(()) => {
                    stored.quota.take();
                    record.pipeline.lock().unwrap().take();
                    false
                }
                Err(_) => {
                    failures += 1;
                    true
                }
            }
        });
        self.cleanup_failures.store(failures, Ordering::Release);
    }
    fn perform(&self, job: &Job) -> Result<DownloadedFile, String> {
        let deadline = Instant::now() + TTL;
        let valid = || {
            Instant::now() < deadline
                && !self.stopped.load(Ordering::Acquire)
                && job.live.load(Ordering::Acquire)
                && (job.current)()
        };
        if !valid() {
            return Err("Attachment download cancelled".into());
        }
        let uri = match &job.source {
            MediaSource::Plain(uri) => uri,
            MediaSource::Encrypted(file) => &file.url,
        };
        // Locally queued plaintext is resolved by the bounded SDK cache adapter.
        if uri
            .server_name()
            .is_ok_and(|server| server.as_str() == "send-queue.localhost")
        {
            return self.local(job, valid, deadline);
        }
        let runtime = crate::sliding_sync::backend_runtime();
        let (record, mut writer) = self.new_file_until(job.current.clone(), deadline)?;
        // Separate admission envelopes reserve ciphertext, verified plaintext
        // and Save As staging together. Actual files retain their own quota
        // until confirmed unlink; failed cleanup never makes disk space free.
        let pipeline = Arc::new(Mutex::new(self.pipelines.reserve(0, MAX_DISK_BYTES)?));
        *record.pipeline.lock().unwrap() = Some(pipeline.clone());
        runtime.block_on(async {
            let mut response = authenticated_response(
                &job.client,
                &job.source,
                &valid,
                job.epoch,
                deadline,
                &self.backing_off,
            )
            .await?;
            if response
                .content_length()
                .is_some_and(|length| length > job.limit)
            {
                return Err("Attachment exceeds its size limit".into());
            }
            let declared = response.content_length();
            if let Some(length) = declared {
                pipeline.lock().unwrap().grow(
                    length
                        .checked_mul(3)
                        .ok_or("Attachment pipeline size overflow")?,
                    MAX_DISK_BYTES,
                )?;
            }
            let started = Instant::now();
            loop {
                if started.elapsed() >= TTL {
                    return Err("Attachment download timed out".into());
                }
                let block = checked(response.chunk(), &valid)
                    .await?
                    .map_err(|error| error.to_string())?;
                let Some(block) = block else {
                    break;
                };
                for part in block.chunks(IO_BUFFER) {
                    if !valid() {
                        return Err("Attachment download cancelled".into());
                    }
                    if declared.is_none() {
                        pipeline
                            .lock()
                            .unwrap()
                            .grow((part.len() as u64) * 3, MAX_DISK_BYTES)?;
                    }
                    self.append(&record, &mut writer, part, job.limit)?;
                }
            }
            Ok::<_, String>(())
        })?;
        writer.file.sync_all().map_err(|error| error.to_string())?;
        drop(writer);
        let output = match &job.source {
            MediaSource::Plain(_) => record,
            MediaSource::Encrypted(info) => {
                let mut source = record.open().map_err(|error| error.to_string())?;
                let mut decryptor = matrix_sdk_crypto::AttachmentDecryptor::new(
                    &mut source,
                    info.as_ref().clone().into(),
                )
                .map_err(|error| error.to_string())?;
                let (plain, mut writer) = self.new_file_until(job.current.clone(), deadline)?;
                *plain.pipeline.lock().unwrap() = Some(pipeline.clone());
                let mut buffer = [0u8; IO_BUFFER];
                loop {
                    if !valid() {
                        return Err("Attachment download cancelled".into());
                    }
                    // The SDK verifies the complete ciphertext hash only at EOF.
                    // No plaintext handle leaves this function before that read.
                    let count = decryptor
                        .read(&mut buffer)
                        .map_err(|error| error.to_string())?;
                    if count == 0 {
                        break;
                    }
                    self.append(&plain, &mut writer, &buffer[..count], job.limit)?;
                }
                writer.file.sync_all().map_err(|error| error.to_string())?;
                plain
            }
        };
        if !valid() {
            return Err("Attachment download cancelled".into());
        }
        output.finalize_at(Instant::now())
    }
    fn local(
        &self,
        job: &Job,
        valid: impl Fn() -> bool,
        deadline: Instant,
    ) -> Result<DownloadedFile, String> {
        static CACHE_RUST_SCRATCH: LazyLock<Arc<ByteBudget>> = LazyLock::new(Arc::default);
        // Keep ordinary local reads serialized through their resulting Vec's
        // original file write, after the cache worker has sent that result.
        // This charges Rust scratch, not SQLite native buffers or process RSS.
        let _memory = CACHE_RUST_SCRATCH.reserve(129 * 1024 * 1024, 129 * 1024 * 1024)?;
        let request = matrix_sdk::media::MediaRequestParameters {
            source: job.source.clone(),
            format: matrix_sdk::media::MediaFormat::File,
        };
        let current = job.current.clone();
        let live = job.live.clone();
        let stopped = self.stopped.clone();
        let cache_current: Current = Arc::new(move || {
            current()
                && live.load(Ordering::Acquire)
                && !stopped.load(Ordering::Acquire)
                && Instant::now() < deadline
        });
        let bytes = crate::sliding_sync::backend_runtime()
            .block_on(crate::media_cache_reader::read(
                job.client.clone(),
                request,
                job.limit.min(64 * 1024 * 1024) as usize,
                cache_current,
            ))?
            .ok_or_else(|| "Locally queued attachment is unavailable".to_owned())?;
        let (record, mut writer) = self.new_file_until(job.current.clone(), deadline)?;
        for chunk in bytes.chunks(IO_BUFFER) {
            if !valid() {
                return Err("Attachment download cancelled".into());
            }
            self.append(&record, &mut writer, chunk, job.limit)?;
        }
        writer.file.sync_all().map_err(|error| error.to_string())?;
        if !valid() {
            return Err("Attachment download cancelled".into());
        }
        record.finalize_at(Instant::now())
    }
}
pub(crate) fn retire() {
    if let Some(service) = SERVICE.get() {
        service.wake.notify_all();
    }
}
pub(crate) fn shutdown() {
    let deadline = Instant::now() + Duration::from_secs(5);
    if let Some(service) = SERVICE.get() {
        service.stopped.store(true, Ordering::Release);
    }
    crate::media_cache_reader::shutdown(deadline);
    io_worker::shutdown(deadline);
    if let Some(service) = SERVICE.get() {
        service.stop(deadline);
    }
}
impl Service {
    fn unfinished_workers(&self) -> Vec<String> {
        self.joins
            .lock()
            .unwrap()
            .iter()
            .filter(|join| !join.is_finished())
            .map(|join| join.thread().name().unwrap_or("unnamed").to_owned())
            .collect()
    }
    fn stop(&self, deadline: Instant) {
        let service = self;
        let mut queue = service.queue.lock().unwrap();
        service.stopped.store(true, Ordering::Release);
        queue.clear();
        service.wake.notify_all();
        loop {
            if service
                .joins
                .lock()
                .unwrap()
                .iter()
                .all(|join| join.is_finished())
            {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                makepad_widgets::error!(
                    "Attachment shutdown remains unconfirmed: active={} bytes={} cleanup_failures={} pending_workers={:?}",
                    service.active.load(Ordering::Acquire),
                    service.disk.used(),
                    cleanup_error_count(service),
                    service.unfinished_workers()
                );
                return;
            }
            queue = service
                .wake
                .wait_timeout(queue, (deadline - now).min(Duration::from_millis(100)))
                .unwrap()
                .0;
        }
        drop(queue);
        for join in service.joins.lock().unwrap().drain(..) {
            if join.join().is_err() {
                makepad_widgets::error!("Original attachment worker failed during shutdown");
            }
        }
    }
}

static SERVICE: OnceLock<Arc<Service>> = OnceLock::new();
fn service() -> &'static Arc<Service> {
    SERVICE.get_or_init(|| Service::start(crate::cache_dir().to_path_buf()))
}
struct Cancel(Arc<AtomicBool>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
#[cfg(test)]
async fn submit(
    service: &Arc<Service>,
    client: Client,
    source: MediaSource,
    limit: u64,
    current: Current,
) -> Result<DownloadedFile, String> {
    submit_at(
        service,
        client,
        source,
        limit,
        current,
        crate::account_session::epoch(),
    )
    .await
}
async fn submit_at(
    service: &Arc<Service>,
    client: Client,
    source: MediaSource,
    limit: u64,
    current: Current,
    epoch: u64,
) -> Result<DownloadedFile, String> {
    let live = Arc::new(AtomicBool::new(true));
    let _cancel = Cancel(live.clone());
    let (done, receiver) = oneshot::channel();
    {
        let mut queue = service.queue.lock().unwrap();
        if service.stopped.load(Ordering::Acquire) {
            return Err("Attachment service is shutting down".into());
        }
        if queue.len() >= 16 {
            return Err("Attachment download queue is full".into());
        }
        queue.push_back(Job {
            epoch,
            client,
            source,
            limit: limit.min(MAX_FILE_BYTES),
            current,
            live,
            done,
        });
        service.wake.notify_one();
    }
    receiver
        .await
        .map_err(|_| "Attachment worker stopped".to_owned())?
}
fn client_scope(client: &Client, epoch: u64) -> Current {
    let original = client.downgrade();
    Arc::new(move || {
        epoch == crate::account_session::epoch()
            && !crate::logout::logout_state_machine::is_logout_in_progress()
            && original
                .get()
                .is_some_and(|client| crate::matrix_context::is_current(&client))
    })
}
pub(crate) async fn download(
    client: Client,
    source: MediaSource,
    limit: u64,
    epoch: u64,
) -> Result<DownloadedFile, String> {
    let current = client_scope(&client, epoch);
    submit_at(service(), client, source, limit, current, epoch).await
}

async fn checked<F: Future>(future: F, valid: &impl Fn() -> bool) -> Result<F::Output, String> {
    tokio::pin!(future);
    let deadline = tokio::time::sleep(Duration::from_secs(60));
    tokio::pin!(deadline);
    loop {
        if !valid() {
            return Err("Attachment download cancelled".into());
        }
        tokio::select! {
            output = &mut future => return Ok(output),
            _ = &mut deadline => return Err("Attachment request timed out".into()),
            _ = tokio::time::sleep(Duration::from_millis(100)) => {},
        }
    }
}
async fn authenticated_response(
    client: &Client,
    source: &MediaSource,
    valid: &impl Fn() -> bool,
    epoch: u64,
    deadline: Instant,
    backing_off: &AtomicUsize,
) -> Result<matrix_sdk::reqwest::Response, String> {
    use matrix_sdk::ruma::api::client::{authenticated_media, media};
    let versions = checked(client.supported_versions(), valid)
        .await?
        .map_err(|error| error.to_string())?;
    let uri = match source {
        MediaSource::Plain(uri) => uri,
        MediaSource::Encrypted(file) => &file.url,
    };
    let mut refreshed = false;
    let mut status_retries = 0u32;
    loop {
        let token = client
            .access_token()
            .ok_or("Matrix access token is unavailable")?;
        let request = if authenticated_media::get_content::v1::Request::PATH_BUILDER
            .is_supported(&versions)
        {
            authenticated_media::get_content::v1::Request::from_uri(uri)
                .map_err(|error| error.to_string())?
                .try_into_http_request::<Vec<u8>>(
                    client.homeserver().as_str(),
                    SendAccessToken::IfRequired(&token),
                    std::borrow::Cow::Borrowed(&versions),
                )
                .map_err(|error| error.to_string())?
        } else {
            #[allow(deprecated)]
            media::get_content::v3::Request::from_url(uri)
                .map_err(|error| error.to_string())?
                .try_into_http_request::<Vec<u8>>(
                    client.homeserver().as_str(),
                    SendAccessToken::IfRequired(&token),
                    std::borrow::Cow::Borrowed(&versions),
                )
                .map_err(|error| error.to_string())?
        };
        let request =
            matrix_sdk::reqwest::Request::try_from(request).map_err(|error| error.to_string())?;
        let mut response = checked(client.http_client().execute(request), valid)
            .await?
            .map_err(|error| error.to_string())?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let header_retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let mut error_body = Vec::new();
        while let Some(chunk) = checked(response.chunk(), valid)
            .await?
            .map_err(|error| error.to_string())?
        {
            if chunk.len() > IO_BUFFER.saturating_sub(error_body.len()) {
                return Err(format!(
                    "Media request failed ({status}); error body too large"
                ));
            }
            error_body.extend_from_slice(&chunk);
        }
        let error_json = serde_json::from_slice::<serde_json::Value>(&error_body).ok();
        let code = error_json
            .as_ref()
            .and_then(|json| json.get("errcode"))
            .and_then(|code| code.as_str());
        let unknown_token = code == Some("M_UNKNOWN_TOKEN");
        if status.as_u16() == 401 && unknown_token && !refreshed {
            refreshed = true;
            if client.access_token().as_ref() == Some(&token) {
                // Refresh may already have consumed a single-use token. The owned
                // worker observes the actual result even if its observer retires.
                if let Err(error) = client.refresh_access_token().await {
                    if refresh_requires_reauthentication(&error) {
                        let soft_logout = serde_json::from_slice::<serde_json::Value>(&error_body)
                            .ok()
                            .and_then(|json| {
                                json.get("soft_logout").and_then(|value| value.as_bool())
                            })
                            .unwrap_or(false);
                        crate::sliding_sync::notify_invalid_matrix_session(
                            client,
                            epoch,
                            soft_logout,
                        );
                    }
                    return Err(format!(
                        "Matrix session refresh failed: {}",
                        crate::session_auth::refresh_failure_code(&error)
                    ));
                }
                if !valid() {
                    return Err("Attachment account retired during session refresh".into());
                }
            }
            continue;
        }
        if code != Some("M_UNRECOGNIZED")
            && (status.as_u16() == 429
                || status.is_server_error()
                || code == Some("M_LIMIT_EXCEEDED"))
        {
            if status_retries >= 3 {
                return Err(format!("Media status retries exhausted ({status})"));
            }
            let matrix_retry_after = error_json
                .as_ref()
                .and_then(|json| json.get("retry_after_ms"))
                .and_then(|value| value.as_u64())
                .map(Duration::from_millis);
            // When both relative hints are supplied, never retry before either.
            let delay = header_retry_after
                .into_iter()
                .chain(matrix_retry_after)
                .max()
                .unwrap_or_else(|| Duration::from_millis(500u64 << status_retries));
            wait_status_retry(delay, deadline, valid, backing_off).await?;
            status_retries += 1;
            continue;
        }
        return Err(format!("Media request failed ({status})"));
    }
}

async fn wait_status_retry(
    delay: Duration,
    deadline: Instant,
    valid: &impl Fn() -> bool,
    backing_off: &AtomicUsize,
) -> Result<(), String> {
    if delay >= deadline.saturating_duration_since(Instant::now()) {
        return Err("Media retry delay exceeds the remaining transfer deadline".into());
    }
    struct Waiting<'a>(&'a AtomicUsize);
    impl Drop for Waiting<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }
    backing_off.fetch_add(1, Ordering::AcqRel);
    let _owned_wait = Waiting(backing_off);
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    loop {
        if !valid() {
            return Err("Attachment download cancelled during retry backoff".into());
        }
        tokio::select! {
            _=&mut sleep=>return Ok(()),
            _=tokio::time::sleep(Duration::from_millis(100))=>{},
        }
    }
}

fn refresh_requires_reauthentication(error: &matrix_sdk::RefreshTokenError) -> bool {
    crate::session_auth::refresh_requires_login(error)
}

fn cleanup_error_count(service: &Service) -> usize {
    service.cleanup_failures.load(Ordering::Acquire)
        + service.creates.failures()
        + service.root.get().map_or(0, |root| {
            root.as_ref().map_or(1, |root| root.cleanup_failures())
        })
}
#[cfg(test)]
fn stats(service: &Service) -> (usize, usize, usize, u64, usize) {
    (
        service.active.load(Ordering::Acquire),
        service.queue.lock().unwrap().len(),
        service.backing_off.load(Ordering::Acquire),
        service.disk.used(),
        cleanup_error_count(service),
    )
}
#[cfg(test)]
#[path = "attachment_transport/tests.rs"]
mod tests;
