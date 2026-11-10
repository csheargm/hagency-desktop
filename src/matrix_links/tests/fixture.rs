use super::*;
use std::sync::Mutex;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

fn sync_event(id: &str, timestamp: u64) -> serde_json::Value {
    serde_json::json!({"event_id":id,"sender":"@alice:example.org","origin_server_ts":timestamp,
        "type":"m.room.message","content":{"msgtype":"m.text","body":id}})
}

pub(crate) type ResponseHandler = dyn Fn(&str) -> Option<(u16, serde_json::Value)> + Send + Sync;

pub(crate) struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    pub(crate) requests: Arc<Mutex<Vec<String>>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Server {
    pub(crate) fn with_handler(context_available: bool, handler: Arc<ResponseHandler>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let done = stop.clone();
        let paths = requests.clone();
        let thread = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let Ok((mut socket, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 8192];
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let Ok(length) = socket.read(&mut buffer) else {
                        break;
                    };
                    if length == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..length]);
                }
                let request = String::from_utf8_lossy(&request);
                let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
                paths.lock().unwrap().push(path.clone());
                let (status, body) = if let Some(response) = handler(&path) {
                    response
                } else if path.contains("/sync?") {
                    (
                        200,
                        serde_json::json!({"next_batch":"s1","rooms":{"join":{"!room:example.org":{
                            "state":{"events":[{"type":"m.room.create","state_key":"","event_id":"$create",
                                "sender":"@alice:example.org","origin_server_ts":1,"content":{"creator":"@alice:example.org","room_version":"10"}},
                                {"type":"m.room.member","state_key":"@alice:example.org","event_id":"$member","sender":"@alice:example.org",
                                "origin_server_ts":2,"content":{"membership":"join"}}]},
                            "timeline":{"events":[sync_event("$recent",1_000_000)],"limited":false,"prev_batch":"older"}
                        }}}}),
                    )
                } else if path.contains("/context/") && context_available {
                    (
                        200,
                        serde_json::json!({"event":sync_event("$ancient",100),"events_before":[sync_event("$before",99)],
                        "events_after":[sync_event("$after",101)],"state":[],"start":"ctx-start","end":"ctx-end"}),
                    )
                } else if path.contains("/members") {
                    (200, serde_json::json!({"chunk":[]}))
                } else {
                    (
                        404,
                        serde_json::json!({"errcode":"M_NOT_FOUND","error":"Unavailable"}),
                    )
                };
                let body = body.to_string();
                let _ = write!(
                    socket,
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self {
            url,
            stop,
            requests,
            thread: Some(thread),
        }
    }
    pub(crate) async fn room_for_links(&self) -> Room {
        self.room_configured(None, true).await
    }
    async fn room_configured(&self, store: Option<&std::path::Path>, threading: bool) -> Room {
        self.room_configured_kind(store, threading, false).await
    }
    async fn room_configured_kind(
        &self,
        store: Option<&std::path::Path>,
        threading: bool,
        space: bool,
    ) -> Room {
        let builder = Client::builder()
            .with_event_cache(true)
            .http_client(
                matrix_sdk::reqwest::Client::builder()
                    .no_proxy()
                    .build()
                    .unwrap(),
            )
            .homeserver_url(&self.url)
            .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_0]);
        let builder = if threading {
            builder
                .with_threading_support(matrix_sdk::ThreadingSupport::Enabled {
                    with_subscriptions: false,
                })
                .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11])
        } else {
            builder
        };
        let builder = if let Some(store) = store {
            builder.sqlite_store(store, Some("fixture-passphrase"))
        } else {
            builder
        };
        let client = builder.build().await.unwrap();
        client
            .matrix_auth()
            .restore_session(
                serde_json::from_value(serde_json::json!({
                    "access_token":"fixture","user_id":"@alice:example.org","device_id":"HISTORY"
                }))
                .unwrap(),
                Default::default(),
            )
            .await
            .unwrap();
        // The upstream SDK subscribes the enabled cache during construction.
        client
            .sync_once(matrix_sdk::config::SyncSettings::default().timeout(Duration::ZERO))
            .await
            .unwrap();
        let room_id = ruma::room_id!("!room:example.org");
        if space {
            let room = client
                .get_room(room_id)
                .expect("actual SDK sync must establish the Space");
            assert!(room.is_space());
            return room;
        }
        let (cache, _handles) = crate::sliding_sync::event_cache(&client).room(room_id).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if cache
                    .events()
                    .await
                    .unwrap()
                    .iter()
                    .any(|event| event.event_id() == Some(ruma::event_id!("$recent")))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        client.get_room(room_id).unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}
