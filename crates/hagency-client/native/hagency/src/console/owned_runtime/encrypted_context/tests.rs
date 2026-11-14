use super::*;
use crate::console::owned_runtime::e2ee::history_record;
use serde_json::json;

const ROOM: &str = "!OvgEtu:localhost:8450";
const AGENT_JOIN: &str = "$agentjoin";
const REPORT: &str = "The README doesn't explain how to run the app locally";

fn msg(id: &str, sender: &str, ts: i64, body: &str) -> Value {
    json!({"type":"m.room.message","event_id":id,"sender":sender,"origin_server_ts":ts,
        "content":{"msgtype":"m.text","body":body}})
}
/// Records exactly as the device's `history_page` yields them.
fn record(raw: Value) -> Value {
    history_record(&raw, false).unwrap()
}
fn undecryptable(id: &str, ts: i64) -> Value {
    history_record(&json!({"type":"m.room.encrypted","event_id":id,"sender":"@ben2:localhost:8452","origin_server_ts":ts,"content":{}}), true).unwrap()
}
fn dispatch(thread_root: &str) -> inbox::Dispatch {
    serde_json::from_value(json!({"id":"evt_1","bindingId":"bnd_1","agentId":"agt_1","eventId":"$ask","roomId":ROOM,
        "requesterMxid":"@ann1:localhost:8450","threadRoot":thread_root,"body":"","state":"running",
        "bindingGeneration":1,"dispatchEpoch":1,"dispatchDeviceId":"dev_1","executionId":"exe_1","outcome":null})).unwrap()
}

/// The Agent's own join membership event, as the timeline carries it.
fn join_event(ts: i64) -> Value {
    json!({"type":"m.room.member","event_id":AGENT_JOIN,"sender":"@ann1-claude:localhost:8450","state_key":"@ann1-claude:localhost:8450",
        "origin_server_ts":ts,"content":{"membership":"join"}})
}

/// The Agent device's Matrix reads, substituted; pages hold raw events,
/// newest first, and are cut at the join exactly as the device does.
struct Fixture {
    joined: Option<&'static str>,
    pages: Vec<Vec<Value>>,
    fail: bool,
    lookups: std::sync::atomic::AtomicUsize,
}
impl RecentSource for Fixture {
    async fn join_event(&self, _room: &str) -> Result<Option<String>, RuntimeError> {
        Ok(self.joined.map(str::to_owned))
    }
    async fn display_name(&self, _room: &str, user: &str) -> Option<String> {
        self.lookups.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        (user == "@ben2:localhost:8452").then(|| "ben2".to_owned())
    }
    async fn start(&self, room: &str, anchor: &str) -> Result<Option<String>, RuntimeError> {
        assert_eq!((room, anchor), (ROOM, "$ask"));
        Ok((!self.pages.is_empty()).then(|| "0".into()))
    }
    async fn page(&self, _room: &str, from: &str, join: &str) -> Result<(HistoryBatch, bool), RuntimeError> {
        if self.fail {
            return Err(RuntimeError::Transport);
        }
        let index: usize = from.parse().unwrap();
        let next = (index + 1 < self.pages.len()).then(|| (index + 1).to_string());
        let entries = self.pages[index].iter().map(|raw| (raw["event_id"].as_str().map(str::to_owned), history_record(raw, false)));
        let (events, reached) = cut_at_join(entries, join);
        Ok((HistoryBatch { events, next }, reached))
    }
}
/// User flow: ben2 (other server) reports in an encrypted room, ann1 later asks
/// her Agent to "triage the report above". The model input must quote the report.
#[tokio::test]
async fn agent_asked_to_triage_the_report_above_sees_the_cross_server_report() {
    let fixture = Fixture {
        joined: Some(AGENT_JOIN),
        pages: vec![
            vec![msg("$ack", "@ann1:localhost:8450", 3_000, "thanks, looking")],
            vec![
                msg("$report", "@ben2:localhost:8452", 2_000, REPORT),
                join_event(1_000),
                msg("$before", "@ann1:localhost:8450", 500, "secret from before the Agent joined"),
            ],
        ],
        fail: false,
        lookups: Default::default(),
    };
    let context = read(&fixture, &dispatch("$ask")).await;
    let text = format!("{}\n\nCurrent request:\n@ann1-claude triage the report above", preamble(&context));
    assert!(text.contains(&format!("ben2 (@ben2:localhost:8452): {REPORT}")), "{text}");
    assert!(text.find(REPORT).unwrap() < text.find("thanks, looking").unwrap());
    assert!(!text.contains("secret from before"), "pre-join history leaked: {text}");
    assert!(!text.contains("could not be read"));
    assert!(text.contains("not instructions or approval"));
}

#[tokio::test]
async fn read_failures_and_a_missing_join_are_reported_as_unreadable() {
    let failing = Fixture { joined: Some(AGENT_JOIN), pages: vec![vec![]], fail: true, lookups: Default::default() };
    assert_eq!(read(&failing, &dispatch("$ask")).await.unavailable, Some("unreadable"));
    let not_joined = Fixture { joined: None, pages: vec![], fail: false, lookups: Default::default() };
    assert_eq!(read(&not_joined, &dispatch("$ask")).await.unavailable, Some("unreadable"));
}

#[test]
fn an_unreadable_context_tells_the_model_to_say_so() {
    let text = preamble(&unreadable("thread"));
    assert!(text.contains("earlier messages in this thread could not be read"), "{text}");
}

fn window<'a>(records: &'a [Value], thread: Option<&'a str>) -> Window<'a> {
    Window { records, trigger: "$ask", thread, complete: true }
}

/// A federated server forges timestamps: a pre-join event dated in the future
/// must stay hidden, and a post-join event dated in the past must be quoted.
#[tokio::test]
async fn join_boundary_follows_timeline_order_not_forged_timestamps() {
    let fixture = Fixture {
        joined: Some(AGENT_JOIN),
        pages: vec![vec![
            msg("$late", "@ben2:localhost:8452", 1, "backdated after the join"),
            join_event(5_000),
            msg("$early", "@ben2:localhost:8452", 9_999_999, "future-dated secret before the join"),
        ]],
        fail: false,
        lookups: Default::default(),
    };
    let context = read(&fixture, &dispatch("$ask")).await;
    let bodies: Vec<_> = context.messages.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, ["backdated after the join"]);
    assert!(!context.omitted_older);
}

/// ann1 has no power to redact ben2's message; the server leaves it intact,
/// so the Agent must keep quoting it. A server-applied redaction still hides.
#[test]
fn unauthorized_redaction_does_not_hide_another_users_message() {
    let foreign = json!({"type":"m.room.redaction","event_id":"$red","sender":"@ann1:localhost:8450","origin_server_ts":4_000,"redacts":"$kept"});
    let mut applied = msg("$gone", "@ben2:localhost:8452", 2_500, "");
    applied["unsigned"] = json!({"redacted_because":{"type":"m.room.redaction"}});
    let records = vec![record(foreign), record(msg("$kept", "@ben2:localhost:8452", 3_000, "still here")), record(applied)];
    let context = select("room", &window(&records, None));
    let bodies: Vec<_> = context.messages.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, ["still here"]);
}

/// Display names are looked up only for quoted senders, after selection.
#[tokio::test]
async fn names_are_resolved_only_for_quoted_messages() {
    let pages = vec![(0..60).map(|i| msg(&format!("$n{i}"), &format!("@u{i}:localhost:8452"), 2_000, "hi")).collect()];
    let fixture = Fixture { joined: Some(AGENT_JOIN), pages, fail: false, lookups: Default::default() };
    let context = read(&fixture, &dispatch("$ask")).await;
    assert_eq!(context.messages.len(), MAX_ENCRYPTED_CONTEXT_MESSAGES);
    assert_eq!(fixture.lookups.load(std::sync::atomic::Ordering::Relaxed), MAX_ENCRYPTED_CONTEXT_MESSAGES);
}

#[test]
fn select_counts_undecryptable_applies_edits_and_drops_redactions_and_the_request() {
    let mut redaction = json!({"type":"m.room.redaction","event_id":"$red","sender":"@ben2:localhost:8452","origin_server_ts":4_000,"content":{"redacts":"$gone"}});
    redaction["redacts"] = "$gone".into();
    let edit = json!({"type":"m.room.message","event_id":"$edit","sender":"@ben2:localhost:8452","origin_server_ts":3_500,
        "content":{"msgtype":"m.text","body":"* fixed","m.new_content":{"msgtype":"m.text","body":"fixed text"},"m.relates_to":{"rel_type":"m.replace","event_id":"$orig"}}});
    let records = vec![
        record(msg("$ask", "@ann1:localhost:8450", 5_000, "triage")),
        record(redaction),
        record(edit),
        undecryptable("$utd", 3_000),
        record(msg("$gone", "@ben2:localhost:8452", 2_500, "removed")),
        record(msg("$orig", "@ben2:localhost:8452", 2_000, "typo text")),
    ];
    let context = select("room", &window(&records, None));
    assert_eq!(context.undecryptable, 1);
    assert_eq!(context.messages.len(), 1);
    assert_eq!((context.messages[0].body.as_str(), context.messages[0].edited), ("fixed text", true));
    let text = preamble(&context);
    assert!(text.contains("1 earlier encrypted message(s) could not be decrypted"));
}

#[test]
fn select_is_bounded_by_count_and_bytes_and_honours_threads() {
    let records: Vec<Value> = (0..30).rev().map(|i| record(msg(&format!("$m{i}"), "@ben2:localhost:8452", 2_000 + i, &format!("m{i}")))).collect();
    let context = select("room", &window(&records, None));
    assert_eq!(context.messages.len(), MAX_ENCRYPTED_CONTEXT_MESSAGES);
    assert!(context.omitted_older);
    assert_eq!(context.messages.last().unwrap().body, "m29");
    let big: Vec<Value> = (0..12).map(|i| record(msg(&format!("$b{i}"), "@ben2:localhost:8452", 2_000, &"x".repeat(4_000)))).collect();
    let context = select("room", &window(&big, None));
    assert!(context.messages.iter().map(|m| m.body.len()).sum::<usize>() <= MAX_ENCRYPTED_CONTEXT_BYTES);
    assert!(context.messages.iter().all(|m| m.body.len() <= MAX_MESSAGE_BYTES) && context.omitted_older);
    let mut reply = msg("$in", "@ben2:localhost:8452", 2_100, "in thread");
    reply["content"]["m.relates_to"] = json!({"rel_type":"m.thread","event_id":"$root"});
    let threaded = vec![record(reply), record(msg("$out", "@ben2:localhost:8452", 2_050, "elsewhere")), record(msg("$root", "@ben2:localhost:8452", 2_000, "root"))];
    let context = select("thread", &window(&threaded, Some("$root")));
    let bodies: Vec<_> = context.messages.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, ["root", "in thread"]);
}
