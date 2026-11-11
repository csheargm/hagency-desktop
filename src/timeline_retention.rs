// SDK timeline residency. Included inside sliding_sync to keep its private owners local.
const MAX_RETAINED_TIMELINES: usize = 16;
static TIMELINE_CREATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
fn timeline_clock() -> u64 {
    static CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    CLOCK.fetch_add(1, Ordering::Relaxed)
}
struct EvictedTimeline {
    sender: crate::timeline_updates::Sender,
    _receiver: Option<crate::timeline_updates::Receiver>,
}
#[derive(Debug)]
pub struct TimelineCacheEvicted(pub TimelineKind, pub usize);

pub(crate) fn update_timeline_visibility(
    kind: &TimelineKind,
    receiver: &crate::timeline_updates::Receiver,
    requests: &TimelineRequestSender,
    open: bool,
) {
    let mut rooms = ALL_JOINED_ROOMS.lock().unwrap();
    if let Some(current) = get_per_timeline_details(&mut rooms, kind) {
        if receiver.shares_route(&current.timeline_update_sender) {
            current
                .request_sender
                .send_modify(|request| request.is_timeline_open = open);
            current.last_used = timeline_clock();
        }
    }
    requests
        .send_if_modified(|request| std::mem::replace(&mut request.is_timeline_open, open) != open);
}

fn reserve_timeline_slot() -> bool {
    let mut rooms = ALL_JOINED_ROOMS.lock().unwrap();
    let count: usize = rooms
        .values()
        .map(|room| usize::from(room.main_timeline.is_some()) + room.thread_timelines.len())
        .sum();
    if count < MAX_RETAINED_TIMELINES {
        return true;
    }
    let candidate = rooms
        .values()
        .flat_map(|room| {
            let main = room.main_timeline.iter().map(|details| {
                (
                    TimelineKind::MainRoom {
                        room_id: room.room_id.clone(),
                    },
                    details,
                )
            });
            let threads = room.thread_timelines.iter().map(|(root, details)| {
                (
                    TimelineKind::Thread {
                        room_id: room.room_id.clone(),
                        thread_root_event_id: root.clone(),
                    },
                    details,
                )
            });
            main.chain(threads)
        })
        .filter(|(_, details)| {
            !details.request_sender.borrow().is_timeline_open
                && details.timeline_singleton_endpoints.is_none()
        })
        .min_by_key(|(_, details)| details.last_used)
        .map(|(kind, _)| kind);
    let Some(kind) = candidate else {
        enqueue_popup_notification(
            "Too many open chat windows. Close one to open this chat.",
            PopupKind::Error,
            Some(6.0),
        );
        return false;
    };
    evict_cached_timeline(&mut rooms, &kind);
    true
}

fn evict_cached_timeline(
    rooms: &mut HashMap<OwnedRoomId, JoinedRoomDetails, ConstHasher>,
    kind: &TimelineKind,
) {
    let room = rooms.get_mut(kind.room_id()).unwrap();
    let mut details = match kind {
        TimelineKind::MainRoom { .. } => {
            for task in room.auxiliary_tasks.drain(..) { task.abort(); }
            room.typing_notice_subscriber.take();
            room.pinned_events_subscriber.take();
            if let Some(task) = room.identity_resets_subscriber.take() {
                task.abort();
            }
            room.main_timeline.take().unwrap()
        }
        TimelineKind::Thread {
            thread_root_event_id,
            ..
        } => room.thread_timelines.remove(thread_root_event_id).unwrap(),
    };
    details.timeline_update_sender.discard_snapshots();
    let channel = details.timeline_update_sender.identity();
    room.evicted_senders.insert(
        kind.clone(),
        EvictedTimeline {
            sender: details.timeline_update_sender.clone(),
            _receiver: details
                .timeline_singleton_endpoints
                .take()
                .map(|(receiver, _)| receiver),
        },
    );
    drop(details);
    Cx::post_action(TimelineCacheEvicted(kind.clone(), channel));
    SignalToUI::set_ui_signal();
}

pub(crate) fn retire_hidden_timeline(kind: &TimelineKind) {
    let mut rooms = ALL_JOINED_ROOMS.lock().unwrap();
    let hidden = get_per_timeline_details(&mut rooms, kind)
        .is_some_and(|details| !details.request_sender.borrow().is_timeline_open);
    if hidden {
        evict_cached_timeline(&mut rooms, kind);
    }
}

async fn ensure_main_timeline(room_id: &RoomId) -> Option<()> {
    let _creation = tokio::time::timeout(Duration::from_secs(30), TIMELINE_CREATION.lock())
        .await
        .ok()?;
    let room = {
        let rooms = ALL_JOINED_ROOMS.lock().unwrap();
        let room = rooms.get(room_id)?;
        if room.main_timeline.is_some() {
            return Some(());
        }
        room.room.clone()
    };
    if !reserve_timeline_slot() {
        return None;
    }
    let client = room.client();
    let timeline = match tokio::time::timeout(
        Duration::from_secs(30),
        room.timeline_builder()
            .event_filter(robrix_timeline_event_filter)
            .with_focus(TimelineFocus::Live {
                hide_threaded_events: true,
            })
            .track_read_marker_and_receipts(TimelineReadReceiptTracking::AllEvents)
            .build(),
    )
    .await
    {
        Ok(Ok(timeline)) => timeline,
        _ => {
            enqueue_popup_notification(
                "Could not load this chat. Please retry.",
                PopupKind::Error,
                Some(6.0),
            );
            return None;
        }
    };
    if !crate::matrix_context::is_current(&client) {
        return None;
    }
    let mut rooms = ALL_JOINED_ROOMS.lock().unwrap();
    let room = rooms.get_mut(room_id)?;
    let kind = TimelineKind::MainRoom {
        room_id: room_id.to_owned(),
    };
    let (sender, receiver) = crate::timeline_updates::channel();
    if let Some(previous) = room.evicted_senders.remove(&kind) {
        previous.sender.forward_operations_to(&sender);
    }
    let (request_sender, request_receiver) = watch::channel(TimelineRequest {
        backwards_paginate: Vec::new(),
        is_timeline_open: false,
    });
    room.main_timeline = Some(PerTimelineDetails {
        timeline: Arc::new(timeline),
        timeline_update_sender: sender,
        timeline_singleton_endpoints: Some((receiver, request_sender.clone())),
        request_sender,
        timeline_subscriber: TimelineSubscriber::NotStarted { request_receiver },
        last_used: timeline_clock(),
        created_at: std::time::Instant::now(),
    });
    Cx::post_action(TimelineEndpointsRecreated {
        room_id: room_id.to_owned(),
    });
    SignalToUI::set_ui_signal();
    Some(())
}

#[cfg(test)]
mod retention_tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path_regex},
    };

    #[test]
    fn real_sdk_lazy_timelines_keep_sixteen_visible_and_reclaim_the_oldest_hidden_window() {
        const CHILD: &str = "HAGENCY_TIMELINE_RETENTION_CHILD";
        const NAME: &str = "sliding_sync::retention_tests::real_sdk_lazy_timelines_keep_sixteen_visible_and_reclaim_the_oldest_hidden_window";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        backend_runtime().block_on(async {
            let server = MockServer::start().await;
            let mut joined = serde_json::Map::new();
            for i in 0..40 {
                joined.insert(format!("!bounded{i}:example.org"), serde_json::json!({
                    "state":{"events":[
                        {"type":"m.room.create","state_key":"","event_id":format!("$create{i}"),"sender":"@alice:example.org","origin_server_ts":1,"content":{"creator":"@alice:example.org","room_version":"10"}},
                        {"type":"m.room.member","state_key":"@alice:example.org","event_id":format!("$join{i}"),"sender":"@alice:example.org","origin_server_ts":2,"content":{"membership":"join"}}
                    ]},
                    "timeline":{"events":[],"limited":false,"prev_batch":"older"}
                }));
            }
            Mock::given(method("GET")).and(path_regex(".*/sync"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"next_batch":"initial","rooms":{"join":joined}})))
                .mount(&server).await;
            let client = Client::builder().homeserver_url(server.uri())
                .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11]).build().await.unwrap();
            client.matrix_auth().restore_session(serde_json::from_value(serde_json::json!({
                "access_token":"fixture", "user_id":"@alice:example.org", "device_id":"RETENTION"
            })).unwrap(), Default::default()).await.unwrap();
            client.sync_once(matrix_sdk::config::SyncSettings::default().timeout(Duration::ZERO)).await.unwrap();
            test_set_current_client(Some(client.clone()));
            for room in client.joined_rooms() {
                ALL_JOINED_ROOMS.lock().unwrap().insert(room.room_id().to_owned(), JoinedRoomDetails {
                    room_id: room.room_id().to_owned(), room, main_timeline: None, evicted_senders: HashMap::new(),
                    thread_timelines: HashMap::new(), pending_thread_timelines: HashSet::new(),
                    typing_notice_subscriber: None, pinned_events_subscriber: None, identity_resets_subscriber: None, auxiliary_tasks: Vec::new(),
                });
            }
            assert_eq!(ALL_JOINED_ROOMS.lock().unwrap().len(), 40);
            assert!(ALL_JOINED_ROOMS.lock().unwrap().values().all(|room| room.main_timeline.is_none()));
            let abandoned: OwnedRoomId = "!bounded39:example.org".try_into().unwrap();
            let owner = makepad_widgets::WidgetRef::empty().widget_uid();
            let abandoned_kind = TimelineKind::MainRoom {room_id: abandoned.clone()};
            note_timeline_interest(&abandoned_kind, owner, true);
            note_timeline_interest(&abandoned_kind, owner, false);
            ensure_requested_main_timeline(&abandoned).await;
            assert!(ALL_JOINED_ROOMS.lock().unwrap().get(&abandoned).unwrap().main_timeline.is_none(), "an abandoned open must not create a controller");
            let mut endpoints = Vec::new();
            for i in 0..16 {
                let id: OwnedRoomId = format!("!bounded{i}:example.org").try_into().unwrap();
                ensure_main_timeline(&id).await.unwrap();
                let kind = TimelineKind::MainRoom {room_id:id};
                endpoints.push((kind.clone(), take_timeline_endpoints(&kind).unwrap()));
            }
            let origin = ALL_JOINED_ROOMS.lock().unwrap().get(endpoints[0].0.room_id()).unwrap().main_timeline.as_ref().unwrap().timeline.clone();
            let old_controller = Arc::downgrade(&origin);
            let held = origin.clone();
            let auxiliary = crate::account_session::spawn(async move {
                let _original = held;
                std::future::pending::<()>().await;
            });
            remember_subscription(endpoints[0].0.room_id(), &origin, &auxiliary);
            drop(origin);
            let extra: OwnedRoomId = "!bounded16:example.org".try_into().unwrap();
            assert!(ensure_main_timeline(&extra).await.is_none(), "visible timelines must not be evicted");
            let (old_kind, old) = &endpoints[0];
            update_timeline_visibility(old_kind, &old.update_receiver, &old.request_sender, false);
            ensure_main_timeline(&extra).await.unwrap();
            assert!(ALL_JOINED_ROOMS.lock().unwrap().get(old_kind.room_id()).unwrap().main_timeline.is_none());
            tokio::time::timeout(Duration::from_secs(5), async {
                while old_controller.upgrade().is_some() { tokio::task::yield_now().await; }
            }).await.expect("original subscriber/auxiliary tasks retained an evicted SDK controller");
            assert!(auxiliary.is_finished());
            assert_eq!(ALL_JOINED_ROOMS.lock().unwrap().values().filter(|room| room.main_timeline.is_some()).count(), 16);
            old.update_sender.send(TimelineUpdate::AttachmentDownloadReset("mxc://example.org/late".into())).unwrap();
            assert!(old.update_sender.send(TimelineUpdate::FirstUpdate {initial_items: Vector::new()}).is_err(), "evicted subscribers cannot republish late snapshots");
            let extra_kind = TimelineKind::MainRoom {room_id:extra.clone()};
            let next = take_timeline_endpoints(&extra_kind).unwrap();
            update_timeline_visibility(&extra_kind, &next.update_receiver, &next.request_sender, false);
            ensure_main_timeline(old_kind.room_id()).await.unwrap();
            let restored = take_timeline_endpoints(old_kind).unwrap();
            assert!(matches!(restored.update_receiver.try_recv(), Ok(TimelineUpdate::AttachmentDownloadReset(uri)) if uri.as_str().ends_with("/late")));
            update_timeline_visibility(old_kind, &restored.update_receiver, &restored.request_sender, false);
            retire_hidden_timeline(old_kind);
            assert!(ALL_JOINED_ROOMS.lock().unwrap().get(old_kind.room_id()).unwrap().main_timeline.is_none(), "hiding releases the SDK subscriber immediately");
            ALL_JOINED_ROOMS.lock().unwrap().clear();
            test_set_current_client(None);
        });
    }
}

/// A cancelled open/late metadata request must not leave an unclaimed SDK
/// timeline running forever. Called on UI timer events; scan at most every 30s.
pub(crate) fn collect_unclaimed_timelines() {
    static LAST: LazyLock<Mutex<std::time::Instant>> =
        LazyLock::new(|| Mutex::new(std::time::Instant::now()));
    let mut last = LAST.lock().unwrap();
    if last.elapsed() < Duration::from_secs(30) {
        return;
    }
    *last = std::time::Instant::now();
    let mut rooms = ALL_JOINED_ROOMS.lock().unwrap();
    let stale: Vec<_> = rooms
        .values()
        .flat_map(|room| {
            let main = room.main_timeline.iter().map(|details| {
                (
                    TimelineKind::MainRoom {
                        room_id: room.room_id.clone(),
                    },
                    details,
                )
            });
            let threads = room.thread_timelines.iter().map(|(root, details)| {
                (
                    TimelineKind::Thread {
                        room_id: room.room_id.clone(),
                        thread_root_event_id: root.clone(),
                    },
                    details,
                )
            });
            main.chain(threads)
        })
        .filter(|(_, details)| {
            details.timeline_singleton_endpoints.is_some()
                && !details.request_sender.borrow().is_timeline_open
                && details.created_at.elapsed() >= Duration::from_secs(30)
        })
        .map(|(kind, _)| kind)
        .collect();
    for kind in stale {
        evict_cached_timeline(&mut rooms, &kind);
    }
}

static TIMELINE_INTEREST: LazyLock<Mutex<HashSet<(TimelineKind, WidgetUid, u64)>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
pub(crate) fn note_timeline_interest(kind: &TimelineKind, owner: WidgetUid, open: bool) {
    let epoch = crate::account_session::epoch();
    let mut interest = TIMELINE_INTEREST.lock().unwrap();
    interest.retain(|(_, _, original)| *original == epoch);
    if open { interest.insert((kind.clone(), owner, epoch)); }
    else { interest.remove(&(kind.clone(), owner, epoch)); }
}
fn has_timeline_interest(kind: &TimelineKind) -> bool {
    let epoch = crate::account_session::epoch();
    TIMELINE_INTEREST.lock().unwrap().iter().any(|(current, _, original)| current == kind && *original == epoch)
}
pub(crate) fn clear_timeline_interest() { TIMELINE_INTEREST.lock().unwrap().clear(); }
async fn ensure_requested_main_timeline(room_id: &RoomId) {
    let kind = TimelineKind::MainRoom {room_id: room_id.to_owned()};
    if !has_timeline_interest(&kind) { return; }
    ensure_main_timeline(room_id).await;
    if !has_timeline_interest(&kind) { retire_hidden_timeline(&kind); }
}
fn remember_subscription(room_id: &RoomId, origin: &Arc<Timeline>, task: &JoinHandle<()>) {
    let mut rooms = ALL_JOINED_ROOMS.lock().unwrap();
    if let Some(room) = rooms.get_mut(room_id) {
        if room.main_timeline.as_ref().is_some_and(|current| Arc::ptr_eq(&current.timeline, origin) && current.request_sender.borrow().is_timeline_open) {
            room.auxiliary_tasks.retain(|task| !task.is_finished());
            room.auxiliary_tasks.push(task.abort_handle());
            return;
        }
    }
    // A late subscription cannot resurrect a hidden/replaced controller.
    task.abort();
}
