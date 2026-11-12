use super::*;
use std::{
    net::{TcpListener, TcpStream},
    sync::{Condvar, mpsc},
    thread::JoinHandle,
};
use matrix_sdk::ruma::api::MatrixVersion;
use serde_json::json;

struct Server {
    url: String,
    stopped: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    requests: Arc<Mutex<Vec<String>>>,
}
impl Server {
    fn new(handler: impl Fn(&str, &mut TcpStream) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let handler = Arc::new(handler);
        let join = std::thread::spawn(move || {
            let mut children = Vec::new();
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        let handler = handler.clone();
                        let observed = observed.clone();
                        children.push(std::thread::spawn(move||{
                            // accept() can inherit O_NONBLOCK from this fixture's
                            // polling listener on macOS. Timeouts do not clear it.
                            socket.set_nonblocking(false).unwrap();
                            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                            socket.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
                            let mut request=Vec::new();let mut byte=[0u8;1];
                            while request.len()<16*1024 && !request.ends_with(b"\r\n\r\n") {
                                match socket.read(&mut byte) {
                                    Ok(0) if request.is_empty()=>return,
                                    Ok(0)=>panic!("fixture peer closed during HTTP headers after {} bytes",request.len()),
                                    Ok(1)=>request.push(byte[0]),
                                    Ok(count)=>unreachable!("one-byte fixture read returned {count}"),
                                    Err(error) if error.kind()==io::ErrorKind::Interrupted=>continue,
                                    Err(error)=>panic!("fixture accepted socket header read failed after {} bytes: {error:?}",request.len()),
                                }
                            }
                            assert!(request.ends_with(b"\r\n\r\n"),"fixture HTTP headers exceeded their bound");
                            let header=String::from_utf8(request.clone()).unwrap();
                            assert!(!header.lines().any(|line|line.to_ascii_lowercase().starts_with("transfer-encoding:")),"fixture expects the SDK's explicit Content-Length: {header}");
                            let length=header.lines().find_map(|line|line.split_once(':').filter(|(name,_)|name.eq_ignore_ascii_case("content-length")).map(|(_,value)|value.trim().parse::<usize>().unwrap())).unwrap_or(0);
                            assert!(length<=64*1024,"fixture request body exceeds bound: {length}");
                            let start=request.len();request.resize(start+length,0);
                            socket.read_exact(&mut request[start..]).expect("consume the complete original SDK HTTP request body before responding/closing");
                            let request=String::from_utf8(request).unwrap();
                            observed.lock().unwrap().push(request.clone());
                            if request.lines().next().unwrap_or("").contains("/download/") || request.starts_with("POST /_matrix/client/v3/refresh") {handler(&request,&mut socket);}
                            else {let _=socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");}
                        }));
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            }
            for child in children {
                child.join().unwrap();
            }
        });
        Self {
            url,
            stopped,
            join: Some(join),
            requests,
        }
    }
    async fn client(&self, refresh: bool) -> Client {
        let client = Client::builder()
            .homeserver_url(&self.url)
            .http_client(
                matrix_sdk::reqwest::Client::builder()
                    .no_proxy()
                    .build()
                    .unwrap(),
            )
            .server_versions([MatrixVersion::V1_11])
            .build()
            .await
            .unwrap();
        let mut session = json!({"access_token":"original-token","user_id":"@attachment:example.org","device_id":"ATTACHMENT"});
        if refresh {
            session["refresh_token"] = json!("original-refresh");
        }
        client
            .matrix_auth()
            .restore_session(serde_json::from_value(session).unwrap(), Default::default())
            .await
            .unwrap();
        client
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.join.take().unwrap().join().unwrap();
    }
}
struct Fixture {
    service: Arc<Service>,
    _cache: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let cache = tempfile::tempdir().unwrap();
        let service = Service::start_budget(cache.path().into(), Arc::default());
        Self {
            service,
            _cache: cache,
        }
    }
    async fn settle(&self) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            self.service.clean();
            if self.service.active.load(Ordering::Acquire) == 0
                && self.service.queue.lock().unwrap().is_empty()
                && self.service.disk.used() == 0
            {
                return;
            }
            assert!(
                Instant::now() < end,
                "original IO/cleanup did not settle: {:?}",
                stats(&self.service)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.service.stop(Instant::now() + Duration::from_secs(5));
    }
}
fn request_paths(server: &Server) -> Vec<String> {
    server
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.lines().next().unwrap_or_default().to_owned())
        .collect()
}
fn assert_refresh_request(request: &str) {
    assert_eq!(
        request.lines().next(),
        Some("POST /_matrix/client/v3/refresh HTTP/1.1"),
        "unexpected actual refresh route"
    );
    let body = request
        .split_once("\r\n\r\n")
        .expect("fixture HTTP body delimiter")
        .1;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body)
            .expect("actual fully consumed SDK refresh JSON"),
        json!({"refresh_token":"original-refresh"})
    );
}
fn plain() -> MediaSource {
    MediaSource::Plain("mxc://example.org/fixture".into())
}
fn reply(socket: &mut TcpStream, bytes: &[u8]) {
    let _ = write!(
        socket,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    for part in bytes.chunks(IO_BUFFER) {
        if socket.write_all(part).is_err() {
            break;
        }
    }
}
async fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < end,
            "fixture did not reach original boundary"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_plain_stream_and_chunked_actual_limit() {
    let bytes = Arc::new(vec![37u8; 3 * IO_BUFFER + 19]);
    let sent = bytes.clone();
    let server = Server::new(move |request, socket| {
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer original-token"));
        reply(socket, &sent);
    });
    let fixture = Fixture::new();
    let file = submit(
        &fixture.service,
        server.client(false).await,
        plain(),
        MAX_FILE_BYTES,
        Arc::new(|| true),
    )
    .await
    .unwrap();
    assert_eq!(file.len(), bytes.len() as u64);
    let mut output = Vec::new();
    file.read_to(&mut output).unwrap();
    assert_eq!(output, *bytes);
    assert_eq!(fixture.service.disk.used(), bytes.len() as u64);
    drop(file);
    fixture.settle().await;
    let server = Server::new(|_, socket| {
        let _ = socket.write_all(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        );
        let _ = socket.write_all(b"8\r\n12345678\r\n8\r\nabcdefgh\r\n0\r\n\r\n");
    });
    assert!(submit(
        &fixture.service,
        server.client(false).await,
        plain(),
        15,
        Arc::new(|| true)
    )
    .await
    .is_err());
    fixture.settle().await;
    let server = Server::new(|_, socket| {
        let _ = socket.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Length: 1073741825\r\nConnection: close\r\n\r\n",
        );
    });
    assert!(submit(
        &fixture.service,
        server.client(false).await,
        plain(),
        MAX_FILE_BYTES,
        Arc::new(|| true)
    )
    .await
    .is_err());
    fixture.settle().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdk_decryptor_requires_complete_ciphertext_integrity_before_publish() {
    let data = vec![53u8; 2 * IO_BUFFER + 7];
    let mut cursor = io::Cursor::new(&data);
    let mut encryptor = matrix_sdk_crypto::AttachmentEncryptor::new(&mut cursor);
    let mut ciphertext = Vec::new();
    encryptor.read_to_end(&mut ciphertext).unwrap();
    let info = encryptor.finish();
    let mut descriptor = serde_json::to_value(info).unwrap();
    descriptor["url"] = json!("mxc://example.org/fixture");
    let source = MediaSource::Encrypted(Box::new(
        serde_json::from_value(descriptor.clone()).unwrap(),
    ));
    let sent = ciphertext.clone();
    let server = Server::new(move |_, socket| reply(socket, &sent));
    let fixture = Fixture::new();
    let file = submit(
        &fixture.service,
        server.client(false).await,
        source.clone(),
        MAX_FILE_BYTES,
        Arc::new(|| true),
    )
    .await
    .unwrap();
    let mut output = Vec::new();
    file.read_to(&mut output).unwrap();
    assert_eq!(output, data);
    drop(file);
    fixture.settle().await;
    *ciphertext.last_mut().unwrap() ^= 1;
    let sent = ciphertext.clone();
    let server = Server::new(move |_, socket| reply(socket, &sent));
    assert!(submit(
        &fixture.service,
        server.client(false).await,
        source,
        MAX_FILE_BYTES,
        Arc::new(|| true)
    )
    .await
    .is_err());
    fixture.settle().await;
    descriptor["hashes"] = json!({});
    let source = MediaSource::Encrypted(Box::new(serde_json::from_value(descriptor).unwrap()));
    assert!(submit(
        &fixture.service,
        server.client(false).await,
        source,
        MAX_FILE_BYTES,
        Arc::new(|| true)
    )
    .await
    .is_err());
    fixture.settle().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn four_original_downloads_and_sixteen_metadata_queue_cancel_without_new_http() {
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let gate = release.clone();
    let active = Arc::new(AtomicUsize::new(0));
    let observed = active.clone();
    let server = Server::new(move |_, socket| {
        observed.fetch_add(1, Ordering::AcqRel);
        let _ =
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n");
        let mut released = gate.0.lock().unwrap();
        while !*released {
            released = gate
                .1
                .wait_timeout(released, Duration::from_secs(5))
                .unwrap()
                .0;
            if !*released {
                return;
            }
        }
        let _ = socket.write_all(b"x");
    });
    let fixture = Fixture::new();
    let client = server.client(false).await;
    let mut jobs = Vec::new();
    for _ in 0..4 {
        let service = fixture.service.clone();
        let client = client.clone();
        jobs.push(tokio::spawn(async move {
            submit(&service, client, plain(), 1, Arc::new(|| true)).await
        }));
    }
    wait(|| active.load(Ordering::Acquire) == 4).await;
    for _ in 0..16 {
        let service = fixture.service.clone();
        let client = client.clone();
        jobs.push(tokio::spawn(async move {
            submit(&service, client, plain(), 1, Arc::new(|| true)).await
        }));
    }
    wait(|| fixture.service.queue.lock().unwrap().len() == 16).await;
    assert!(
        submit(&fixture.service, client, plain(), 1, Arc::new(|| true))
            .await
            .is_err()
    );
    // Tokio abort requests cancellation; joining confirms that the runtime has
    // dropped the future and its Cancel guard. Retire queued observers before
    // freeing the four occupied workers, or a still-live queued job can
    // start HTTP while cancellation of the earlier active jobs is processed.
    let queued = jobs.split_off(4);
    for job in &queued {
        job.abort();
    }
    for job in queued {
        assert!(matches!(job.await, Err(error) if error.is_cancelled()));
    }
    assert_eq!(active.load(Ordering::Acquire), 4);
    for job in &jobs {
        job.abort();
    }
    for job in jobs {
        let _ = job.await;
    }
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    fixture.settle().await;
    assert_eq!(
        active.load(Ordering::Acquire),
        4,
        "cancelled queued jobs must not start another HTTP request"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_keeps_blocked_original_reader_charged_then_removes_held_ui_file() {
    let server = Server::new(|_, socket| reply(socket, &vec![17u8; 2 * IO_BUFFER]));
    let fixture = Fixture::new();
    let current = Arc::new(AtomicBool::new(true));
    let scope = current.clone();
    let file = submit(
        &fixture.service,
        server.client(false).await,
        plain(),
        MAX_FILE_BYTES,
        Arc::new(move || scope.load(Ordering::Acquire)),
    )
    .await
    .unwrap();
    let held = file.clone();
    let reader = file.clone();
    let (entered, observed) = mpsc::sync_channel(1);
    let (release, waiting) = mpsc::sync_channel(1);
    struct Sink {
        entered: Option<mpsc::SyncSender<()>>,
        release: mpsc::Receiver<()>,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
                self.release.recv().unwrap();
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let original = std::thread::spawn(move || {
        reader.read_to(&mut Sink {
            entered: Some(entered),
            release: waiting,
        })
    });
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    current.store(false, Ordering::Release);
    fixture.service.clean();
    assert_eq!(
        fixture.service.disk.used(),
        2 * IO_BUFFER as u64,
        "retirement cannot release blocked original IO quota"
    );
    assert!(!file.is_current());
    release.send(()).unwrap();
    assert!(original.join().unwrap().is_err());
    fixture.settle().await;
    assert!(held.read_to(&mut io::sink()).is_err());
    drop((file, held));
}
#[test]
fn production_record_limit_ttl_and_confirmed_cleanup_own_the_quota() {
    let fixture = Fixture::new();
    let mut held = Vec::new();
    for _ in 0..64 {
        let (record, writer) = fixture.service.new_file(Arc::new(|| true)).unwrap();
        drop(writer);
        held.push(record);
    }
    assert!(fixture.service.new_file(Arc::new(|| true)).is_err());
    held.clear();
    fixture.service.clean();
    let (record, mut writer) = fixture
        .service
        .new_file_until(Arc::new(|| true), Instant::now() - Duration::from_secs(1))
        .unwrap();
    fixture
        .service
        .append(&record, &mut writer, b"secret", 10)
        .unwrap();
    fixture.service.clean();
    assert_eq!(fixture.service.disk.used(), 6);
    drop(writer);
    fixture.service.clean();
    assert_eq!(fixture.service.disk.used(), 0);
    assert!(record.open().is_err());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_refresh_is_observed_after_download_observer_cancels() {
    let refreshing = Arc::new(AtomicBool::new(false));
    let observed = refreshing.clone();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let gate = release.clone();
    let server = Server::new(move |request, socket| {
        if request.starts_with("POST") {
            assert_refresh_request(request);
            observed.store(true, Ordering::Release);
            let mut released = gate.0.lock().unwrap();
            while !*released {
                released = gate
                    .1
                    .wait_timeout(released, Duration::from_secs(5))
                    .unwrap()
                    .0;
                if !*released {
                    return;
                }
            }
            let body=b"{\"access_token\":\"refreshed-token\",\"refresh_token\":\"refreshed-refresh\",\"expires_in_ms\":3600000}";
            let _ = write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(body);
        } else {
            let body = b"{\"errcode\":\"M_UNKNOWN_TOKEN\",\"error\":\"expired\"}";
            let _ = write!(
                socket,
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(body);
        }
    });
    let fixture = Fixture::new();
    let client = server.client(true).await;
    let original = client.clone();
    let service = fixture.service.clone();
    let task =
        tokio::spawn(
            async move { submit(&service, original, plain(), 100, Arc::new(|| true)).await },
        );
    wait(|| refreshing.load(Ordering::Acquire)).await;
    task.abort();
    let _ = task.await;
    assert_eq!(fixture.service.active.load(Ordering::Acquire), 1);
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    fixture.settle().await;
    assert_eq!(
        client.access_token().as_deref(),
        Some("refreshed-token"),
        "actual refresh failed: worker_error={:?}; request_paths={:?}",
        fixture.service.last_error.lock().unwrap(),
        request_paths(&server)
    );
    assert_eq!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.lines().next().unwrap_or("").contains("/download/"))
            .count(),
        1,
        "cancelled observer must not trigger a second media request after refresh"
    );
}
#[test]
fn save_as_journal_recovers_crash_and_preserves_published_user_file() {
    let cache = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let target = output.path().join("chosen.bin");
    let root = files::Root::new(cache.path()).unwrap();
    let (scratch, mut file) = root.destination(&target).unwrap();
    let path = scratch.path_for_test();
    file.write_all(b"private partial").unwrap();
    file.sync_all().unwrap();
    drop(file);
    drop(scratch);
    drop(root);
    let root = files::Root::new(cache.path()).unwrap();
    assert!(!path.exists());
    let (mut scratch, mut file) = root.destination(&target).unwrap();
    file.write_all(b"saved").unwrap();
    file.sync_all().unwrap();
    drop(file);
    scratch.publish(&target).unwrap();
    drop(scratch);
    drop(root);
    let _root = files::Root::new(cache.path()).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"saved");
}

#[test]
fn save_as_partial_journal_setup_failure_removes_original_metadata_and_retries() {
    let cache = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let target = output.path().join("chosen.bin");
    let root = files::Root::new(cache.path()).unwrap();
    assert!(root
        .destination_with(&target, |file, _, _| {
            file.write_all(b"{")?;
            Err(io::Error::other("injected journal sync failure"))
        })
        .is_err());
    assert_eq!(
        std::fs::read_dir(output.path()).unwrap().count(),
        0,
        "failed setup must not leave target scratch"
    );
    drop(root);
    let root = files::Root::new(cache.path()).unwrap();
    let (mut scratch, file) = root.destination(&target).unwrap();
    drop(file);
    scratch.remove().unwrap();
}
#[test]
fn save_as_crash_recovery_refuses_replaced_file_and_symlink_parent() {
    let cache = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let target = output.path().join("chosen.bin");
    let root = files::Root::new(cache.path()).unwrap();
    let (scratch, mut file) = root.destination(&target).unwrap();
    let path = scratch.path_for_test();
    file.write_all(b"original private").unwrap();
    drop(file);
    drop(scratch);
    drop(root);
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"unrelated replacement").unwrap();
    assert!(files::Root::new(cache.path()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"unrelated replacement");
    #[cfg(unix)]
    {
        let cache = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let parent = output.path().join("parent");
        std::fs::create_dir(&parent).unwrap();
        let root = files::Root::new(cache.path()).unwrap();
        let (scratch, file) = root.destination(&parent.join("target")).unwrap();
        let path = scratch.path_for_test();
        drop(file);
        drop(scratch);
        drop(root);
        std::fs::rename(&parent, output.path().join("original-parent")).unwrap();
        std::os::unix::fs::symlink(output.path().join("original-parent"), &parent).unwrap();
        assert!(files::Root::new(cache.path()).is_err());
        assert!(
            path.exists(),
            "recovery must not follow substituted parent link"
        );
    }
}
#[test]
fn shutdown_does_not_wait_for_records_lock_held_by_original_filesystem_work() {
    let fixture = Fixture::new();
    let service = fixture.service.clone();
    let (entered, observed) = mpsc::sync_channel(1);
    let (release, waiting) = mpsc::sync_channel(1);
    let original = std::thread::spawn(move || {
        let _records = service.records.lock().unwrap();
        entered.send(()).unwrap();
        waiting.recv_timeout(Duration::from_millis(250)).is_ok()
    });
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    let started = Instant::now();
    fixture.service.stop(started + Duration::from_millis(50));
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "UI shutdown may report original IO unknown, but cannot block on its registry lock"
    );
    let pending = fixture.service.unfinished_workers();
    assert!(
        pending.iter().any(|name| name == "attachment-cleanup"),
        "short shutdown must report the actual still-owned cleanup thread: {pending:?}"
    );
    release.send(()).unwrap();
    assert!(
        original.join().unwrap(),
        "a blocking shutdown would only return after the external holder's 250ms fail-safe release"
    );
    fixture
        .service
        .stop(Instant::now() + Duration::from_secs(1));
    assert!(
        fixture.service.joins.lock().unwrap().is_empty(),
        "after releasing original filesystem custody every actual thread must be joined, not just counted inactive"
    );
}
#[test]
fn streaming_refresh_keeps_sdk_invalid_grant_vs_transient_failure_policy() {
    use matrix_sdk::{
        RefreshTokenError,
        authentication::oauth::{
            OAuthError,
            error::{RequestTokenError, StandardErrorResponse, BasicErrorResponseType},
        },
    };
    let invalid = RefreshTokenError::OAuth(Arc::new(OAuthError::RefreshToken(
        RequestTokenError::ServerResponse(StandardErrorResponse::new(
            BasicErrorResponseType::InvalidGrant,
            None,
            None,
        )),
    )));
    assert!(refresh_requires_reauthentication(&invalid));
    let temporary = RefreshTokenError::OAuth(Arc::new(OAuthError::RefreshToken(
        RequestTokenError::Other("temporary OAuth network failure".into()),
    )));
    assert!(!refresh_requires_reauthentication(&temporary));
    assert!(refresh_requires_reauthentication(
        &RefreshTokenError::RefreshTokenRequired
    ));
}

#[test]
fn media_refresh_ui_failure_never_copies_remote_secret_descriptions() {
    use matrix_sdk::{
        RefreshTokenError,
        authentication::oauth::{OAuthError, error::RequestTokenError},
    };
    let response=serde_json::from_value(json!({"error":"invalid_grant","error_description":"REFRESH_SECRET_MARKER","error_uri":"https://example.org/REFRESH_SECRET_MARKER"})).unwrap();
    let error = RefreshTokenError::OAuth(Arc::new(OAuthError::RefreshToken(
        RequestTokenError::ServerResponse(response),
    )));
    let ui = format!(
        "Matrix session refresh failed: {}",
        crate::session_auth::refresh_failure_code(&error)
    );
    assert!(!ui.contains("REFRESH_SECRET_MARKER"));
    assert!(ui.contains("matrix_authorization_required"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_failed_refresh_returns_stable_ui_code_without_remote_secret_text() {
    let server = Server::new(|request, socket| {
        let body = if request.starts_with("POST") {
            assert_refresh_request(request);
            b"{\"errcode\":\"M_UNKNOWN_TOKEN\",\"error\":\"REFRESH_SECRET_MARKER\"}".as_slice()
        } else {
            b"{\"errcode\":\"M_UNKNOWN_TOKEN\",\"error\":\"expired\"}".as_slice()
        };
        let _ = write!(
            socket,
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = socket.write_all(body);
    });
    let fixture = Fixture::new();
    let error = submit(
        &fixture.service,
        server.client(true).await,
        plain(),
        100,
        Arc::new(|| true),
    )
    .await
    .err()
    .unwrap();
    assert!(!error.contains("REFRESH_SECRET_MARKER"));
    assert!(
        error.contains("matrix_authorization_required"),
        "actual worker error={error:?}; request_paths={:?}",
        request_paths(&server)
    );
    fixture.settle().await;
}

#[tokio::test]
async fn terminal_auth_notification_rejects_retired_operation_before_client_replacement() {
    let client = Client::builder()
        .homeserver_url("http://127.0.0.1:1")
        .server_versions([MatrixVersion::V1_11])
        .build()
        .await
        .unwrap();
    assert!(!crate::sliding_sync::notify_invalid_matrix_session(
        &client,
        u64::MAX,
        true
    ));
}

#[test]
fn finalize_gives_slow_verified_transfer_one_full_usable_ttl_without_extending_inflight() {
    let fixture = Fixture::new();
    let began = Instant::now();
    let deadline = began + TTL;
    let (record, mut writer) = fixture
        .service
        .new_file_until(Arc::new(|| true), deadline)
        .unwrap();
    fixture
        .service
        .append(&record, &mut writer, b"verified bytes", 100)
        .unwrap();
    writer.file.sync_all().unwrap();
    drop(writer);
    let unpublished = DownloadedFile(record.clone());
    assert!(
        !unpublished.is_current_at(deadline - Duration::from_millis(1)),
        "no usable plaintext is published before the producer's verified EOF/finalize boundary"
    );
    let completed = deadline - Duration::from_millis(1);
    let usable = record.finalize_at(completed).unwrap();
    assert_eq!(record.expires_at(), completed + TTL);
    assert!(
        usable.is_current_at(completed + TTL - Duration::from_millis(1)),
        "a transfer completing near its original deadline still receives a complete usable TTL"
    );
    assert!(!usable.is_current_at(completed + TTL));
    assert!(
        record
            .finalize_at(completed + Duration::from_secs(1))
            .is_err(),
        "repeated use cannot extend the once-only TTL"
    );
    let (late, writer) = fixture
        .service
        .new_file_until(Arc::new(|| true), deadline)
        .unwrap();
    drop(writer);
    assert!(
        late.finalize_at(deadline).is_err(),
        "completion after the fixed in-flight deadline cannot publish"
    );
    assert!(!DownloadedFile(late).is_current_at(deadline));
}

#[test]
fn accepted_fixture_socket_waits_for_delayed_client_headers_instead_of_silent_would_block() {
    let server = Server::new(|_, socket| reply(socket, b"delayed headers accepted"));
    let address = server.url.strip_prefix("http://").unwrap();
    let mut socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    // Give the accepted worker time to reach its first read with no bytes yet.
    // An inherited nonblocking socket previously returned WouldBlock and closed.
    std::thread::sleep(Duration::from_millis(30));
    socket.write_all(b"GET /_matrix/client/v1/media/download/example.org/fixture HTTP/1.1\r\nHost: example.org\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = Vec::new();
    socket.take(1024).read_to_end(&mut response).unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200 OK"));
    assert!(response.ends_with(b"delayed headers accepted"));
    assert_eq!(request_paths(&server).len(), 1);
}
fn status_reply(socket: &mut TcpStream, status: &str, extra_headers: &str, body: &[u8]) {
    write!(socket,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n",body.len()).unwrap();
    socket.write_all(body).unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_503_retries_original_get_then_publishes_verified_file() {
    let gets = Arc::new(AtomicUsize::new(0));
    let observed = gets.clone();
    let server = Server::new(move |_, socket| {
        if observed.fetch_add(1, Ordering::AcqRel) == 0 {
            status_reply(
                socket,
                "503 Service Unavailable",
                "",
                br#"{"errcode":"M_UNKNOWN","error":"remote description"}"#,
            );
        } else {
            reply(socket, b"recovered 503");
        }
    });
    let fixture = Fixture::new();
    let file = submit(
        &fixture.service,
        server.client(false).await,
        plain(),
        100,
        Arc::new(|| true),
    )
    .await
    .unwrap();
    assert_eq!(file.read_small(100).unwrap(), b"recovered 503");
    assert_eq!(gets.load(Ordering::Acquire), 2);
    drop(file);
    fixture.settle().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_limit_honors_relative_header_and_matrix_delay_before_next_get() {
    let times = Arc::new(Mutex::new(Vec::new()));
    let observed = times.clone();
    let server = Server::new(move |_, socket| {
        let count = {
            let mut times = observed.lock().unwrap();
            times.push(Instant::now());
            times.len()
        };
        if count == 1 {
            status_reply(socket,"429 Too Many Requests","Retry-After: 0\r\n",br#"{"errcode":"M_LIMIT_EXCEEDED","retry_after_ms":50,"error":"remote description"}"#);
        } else {
            reply(socket, b"recovered 429");
        }
    });
    let fixture = Fixture::new();
    let file = submit(
        &fixture.service,
        server.client(false).await,
        plain(),
        100,
        Arc::new(|| true),
    )
    .await
    .unwrap();
    {
        let times = times.lock().unwrap();
        assert_eq!(times.len(), 2);
        assert!(times[1].duration_since(times[0]) >= Duration::from_millis(50));
    }
    assert_eq!(file.read_small(100).unwrap(), b"recovered 429");
    drop(file);
    fixture.settle().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recoverable_status_retries_are_finite_and_unrecognized_is_permanent() {
    let gets = Arc::new(AtomicUsize::new(0));
    let observed = gets.clone();
    let server = Server::new(move |_, socket| {
        observed.fetch_add(1, Ordering::AcqRel);
        status_reply(
            socket,
            "503 Service Unavailable",
            "",
            br#"{"errcode":"M_UNKNOWN","error":"SECRET_STATUS_DESCRIPTION"}"#,
        );
    });
    let fixture = Fixture::new();
    let error = submit(
        &fixture.service,
        server.client(false).await,
        plain(),
        100,
        Arc::new(|| true),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(gets.load(Ordering::Acquire), 4);
    assert!(error.contains("status retries exhausted"));
    assert!(!error.contains("SECRET_STATUS_DESCRIPTION"));
    fixture.settle().await;
    let gets = Arc::new(AtomicUsize::new(0));
    let observed = gets.clone();
    let server = Server::new(move |_, socket| {
        observed.fetch_add(1, Ordering::AcqRel);
        status_reply(
            socket,
            "503 Service Unavailable",
            "",
            br#"{"errcode":"M_UNRECOGNIZED","error":"SECRET_STATUS_DESCRIPTION"}"#,
        );
    });
    let error = submit(
        &fixture.service,
        server.client(false).await,
        plain(),
        100,
        Arc::new(|| true),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(gets.load(Ordering::Acquire), 1);
    assert!(error.contains("503"));
    assert!(!error.contains("SECRET_STATUS_DESCRIPTION"));
    fixture.settle().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_after_exceeding_original_deadline_fails_without_early_server_retry() {
    let gets = Arc::new(AtomicUsize::new(0));
    let observed = gets.clone();
    let server = Server::new(move |_, socket| {
        observed.fetch_add(1, Ordering::AcqRel);
        status_reply(
            socket,
            "429 Too Many Requests",
            "Retry-After: 3600\r\n",
            br#"{"errcode":"M_LIMIT_EXCEEDED"}"#,
        );
    });
    let fixture = Fixture::new();
    let error = submit(
        &fixture.service,
        server.client(false).await,
        plain(),
        100,
        Arc::new(|| true),
    )
    .await
    .err()
    .unwrap();
    assert!(error.contains("retry delay exceeds"));
    assert_eq!(gets.load(Ordering::Acquire), 1);
    assert_eq!(fixture.service.backing_off.load(Ordering::Acquire), 0);
    fixture.settle().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_during_observed_original_backoff_never_sends_another_get() {
    let gets = Arc::new(AtomicUsize::new(0));
    let observed = gets.clone();
    let server = Server::new(move |_, socket| {
        observed.fetch_add(1, Ordering::AcqRel);
        status_reply(
            socket,
            "429 Too Many Requests",
            "Retry-After: 120\r\n",
            br#"{"errcode":"M_LIMIT_EXCEEDED"}"#,
        );
    });
    let fixture = Fixture::new();
    let client = server.client(false).await;
    let service = fixture.service.clone();
    let observer =
        tokio::spawn(
            async move { submit(&service, client, plain(), 100, Arc::new(|| true)).await },
        );
    wait(|| fixture.service.backing_off.load(Ordering::Acquire) == 1).await;
    assert_eq!(gets.load(Ordering::Acquire), 1);
    observer.abort();
    let _ = observer.await;
    fixture.settle().await;
    assert_eq!(gets.load(Ordering::Acquire), 1);
    assert_eq!(fixture.service.backing_off.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn idle_record_scope_does_not_keep_retired_sdk_client_alive() {
    let client = Client::builder()
        .homeserver_url("http://127.0.0.1:1")
        .server_versions([MatrixVersion::V1_11])
        .build()
        .await
        .unwrap();
    let weak = client.downgrade();
    let scope = client_scope(&client, crate::account_session::epoch());
    let fixture = Fixture::new();
    let (held_record, writer) = fixture.service.new_file(scope).unwrap();
    assert!(weak.get().is_some());
    drop(client);
    assert!(
        weak.get().is_none(),
        "a native dialog's idle record may retain file custody but must not retain the retired SDK/store"
    );
    assert!(!(held_record.current)());
    drop(writer);
}

#[test]
fn private_create_seal_failure_cleans_zero_byte_journal_and_destination_names() {
    let cache = tempfile::tempdir().unwrap();
    let owner = Arc::new(files::CreateOwner::default());
    let root = files::Root::new_with_owner(cache.path(), owner.clone()).unwrap();
    let directory = root.dir_for_test().unwrap();
    for name in [
        "save_injected.json",
        ".hagency-download-injected",
        "file_injected",
    ] {
        assert!(files::create_with(
            &owner,
            &directory,
            Path::new(name),
            |file| {
                assert_eq!(file.metadata()?.len(), 0);
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected private seal failure",
                ))
            },
            |parent, name| parent.remove_file(name)
        )
        .is_err());
        assert!(directory.symlink_metadata(name).is_err());
        assert_eq!(owner.failures(), 0);
    }
    drop(directory);
    drop(root);
    let _reopened = files::Root::new_with_owner(cache.path(), owner).unwrap();
}
#[test]
fn private_create_unknown_unlink_keeps_original_witness_and_recovers_after_retry() {
    let cache = tempfile::tempdir().unwrap();
    let owner = Arc::new(files::CreateOwner::default());
    let root = files::Root::new_with_owner(cache.path(), owner.clone()).unwrap();
    let directory = root.dir_for_test().unwrap();
    assert!(files::create_with(
        &owner,
        &directory,
        Path::new("save_injected.json"),
        |_| Err(io::Error::other("injected seal")),
        |_, _| Err(io::Error::other("injected busy unlink"))
    )
    .is_err());
    assert_eq!(owner.failures(), 1);
    assert_eq!(directory.metadata("save_injected.json").unwrap().len(), 0);
    assert!(files::create_with(
        &owner,
        &directory,
        Path::new("must_not_create"),
        |_| Ok(()),
        |parent, name| parent.remove_file(name)
    )
    .is_err());
    assert!(directory.symlink_metadata("must_not_create").is_err());
    owner.retry();
    assert_eq!(owner.failures(), 0);
    assert!(directory.symlink_metadata("save_injected.json").is_err());
    let guard = files::create_with(
        &owner,
        &directory,
        Path::new("post_seal_setup_failure"),
        |file| hagency_store::private::seal_created_file_handle(file).map_err(io::Error::other),
        |parent, name| parent.remove_file(name),
    )
    .unwrap();
    // The created guard remains armed through fallible Scratch/JournalRef setup.
    drop(guard);
    assert!(directory
        .symlink_metadata("post_seal_setup_failure")
        .is_err());
    drop(directory);
    drop(root);
    let _reopened = files::Root::new_with_owner(cache.path(), owner).unwrap();
}
#[test]
fn private_create_cleanup_refuses_replacement_and_retains_original_identity_for_retry() {
    let cache = tempfile::tempdir().unwrap();
    let owner = Arc::new(files::CreateOwner::default());
    let root = files::Root::new_with_owner(cache.path(), owner.clone()).unwrap();
    let directory = root.dir_for_test().unwrap();
    assert!(files::create_with(
        &owner,
        &directory,
        Path::new("created"),
        |_| {
            directory.rename("created", &directory, "original").unwrap();
            directory
                .write("created", b"replacement must survive")
                .unwrap();
            Err(io::Error::other("injected private seal failure"))
        },
        |parent, name| parent.remove_file(name)
    )
    .is_err());
    assert_eq!(owner.failures(), 1);
    owner.retry();
    assert_eq!(owner.failures(), 1);
    assert_eq!(
        directory.read("created").unwrap(),
        b"replacement must survive"
    );
    directory.remove_file("created").unwrap();
    directory.rename("original", &directory, "created").unwrap();
    owner.retry();
    assert_eq!(owner.failures(), 0);
    assert!(directory.symlink_metadata("created").is_err());
}

#[test]
fn private_create_inflight_cleanup_keeps_atomic_diagnostic_and_admission_blocked() {
    let cache = tempfile::tempdir().unwrap();
    let owner = Arc::new(files::CreateOwner::default());
    let root = files::Root::new_with_owner(cache.path(), owner.clone()).unwrap();
    let directory = root.dir_for_test().unwrap();
    assert!(files::create_with(
        &owner,
        &directory,
        Path::new("created"),
        |_| Err(io::Error::other("injected seal failure")),
        |_, _| Err(io::Error::other("injected initial unlink failure"))
    )
    .is_err());
    let original = owner.clone();
    let (entered, holding) = mpsc::sync_channel(1);
    let (release, waiting) = mpsc::sync_channel(1);
    let retry = std::thread::spawn(move || {
        original.retry_with(|parent, name| {
            entered.send(()).unwrap();
            assert!(
                waiting.recv_timeout(Duration::from_millis(250)).is_ok(),
                "the atomic diagnostic/admission read blocked on original cleanup IO"
            );
            parent.remove_file(name)
        })
    });
    holding.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(
        owner.failures(),
        1,
        "taking the pending vector does not retire its still-owned witness"
    );
    owner.retry();
    assert_eq!(
        owner.failures(),
        1,
        "a concurrent empty retry cannot clear an in-flight witness count"
    );
    assert!(files::create_with(
        &owner,
        &directory,
        Path::new("must_not_create"),
        |_| Ok(()),
        |parent, name| parent.remove_file(name)
    )
    .is_err());
    assert!(directory.symlink_metadata("must_not_create").is_err());
    release.send(()).unwrap();
    retry.join().unwrap();
    assert_eq!(owner.failures(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_above_one_gib_streams_and_saves_without_a_payload_vector() {
    const LENGTH: u64 = 1024 * 1024 * 1024 + IO_BUFFER as u64;
    let server = Server::new(|_, socket| {
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {LENGTH}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let block = [73u8; IO_BUFFER];
        for _ in 0..LENGTH / IO_BUFFER as u64 {
            socket.write_all(&block).unwrap();
        }
    });
    let fixture = Fixture::new();
    let file = submit(
        &fixture.service,
        server.client(false).await,
        plain(),
        MAX_FILE_BYTES,
        Arc::new(|| true),
    )
    .await
    .unwrap();
    assert_eq!(file.len(), LENGTH);
    assert_eq!(fixture.service.pipelines.used(), LENGTH * 3);
    struct Verify(u64);
    impl Write for Verify {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            assert!(bytes.iter().all(|byte| *byte == 73));
            self.0 += bytes.len() as u64;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut verify = Verify(0);
    file.read_to(&mut verify).unwrap();
    assert_eq!(verify.0, LENGTH);
    let target = fixture._cache.path().join("saved-large-file");
    file.save_with(&fixture.service, &target).unwrap();
    assert_eq!(std::fs::metadata(&target).unwrap().len(), LENGTH);
    drop(file);
    fixture.settle().await;
    assert_eq!(fixture.service.pipelines.used(), 0);
    assert!(
        target.exists(),
        "user-saved files survive temporary cache cleanup"
    );
    std::fs::remove_file(target).unwrap();
}

#[tokio::test]
async fn declared_pipeline_that_cannot_fit_is_rejected_before_consuming_the_body() {
    let server = Server::new(|_, socket| {
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", MAX_DISK_BYTES / 3 + 1).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    });
    let fixture = Fixture::new();
    let result = submit(&fixture.service, server.client(false).await, plain(), MAX_FILE_BYTES, Arc::new(|| true)).await;
    let error = result.err().expect("pipeline must not be admitted");
    assert!(error.contains("budget"), "{error}");
    fixture.settle().await;
    assert_eq!(fixture.service.pipelines.used(), 0);
}
