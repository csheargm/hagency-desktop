use super::*;

fn memory(id: &str, sender: &str, text: &str, rev: u64) -> Value {
    json!({"type":MEMORY_STATE_TYPE,"sender":sender,"content":{"id":id,"text":text,"rev":rev}})
}
fn page(events: Vec<ScanKind>, next: Option<&str>) -> ScanPage {
    let events = events.into_iter().enumerate().map(|(i, k)| (format!("$e{i}"), k)).collect();
    ScanPage { events, next: next.map(Into::into) }
}
fn boss() -> PowerLevels {
    PowerLevels { users: [("@boss:x".to_string(), 50)].into(), ..Default::default() }
}

#[test]
fn scan_reads_whole_short_room_oldest_first_and_counts_unreadable() {
    let mut state = ScanState::default();
    let step = state.push(page(
        vec![ScanKind::Memory(memory("a", "@boss:x", "new", 2)), ScanKind::Unreadable, ScanKind::Other],
        Some("t1"),
    ));
    assert!(matches!(step, ScanStep::Next(ref t) if t == "t1"));
    assert!(matches!(state.push(page(vec![ScanKind::Memory(memory("a", "@boss:x", "old", 1))], None)), ScanStep::Done));
    let scan = state.finish();
    assert_eq!((scan.unreadable, scan.truncated, scan.newest.as_deref()), (1, false, Some("$e0")));
    assert_eq!(scan.events[0]["content"]["text"], "old", "oldest first");
}

#[test]
fn page_cap_stops_and_reports_truncation() {
    let mut state = ScanState::default();
    for i in 0..SCAN_MAX_PAGES {
        match state.push(page(vec![ScanKind::Other], Some(&format!("t{i}")))) {
            ScanStep::Next(_) => assert!(i + 1 < SCAN_MAX_PAGES),
            ScanStep::Done => assert_eq!(i + 1, SCAN_MAX_PAGES),
        }
    }
    assert!(state.finish().truncated);
}

#[test]
fn event_cap_byte_cap_and_stuck_token_hold() {
    let mut state = ScanState::default();
    let many = (0..SCAN_MAX_EVENTS + 5).map(|_| ScanKind::Other).collect();
    assert!(matches!(state.push(page(many, Some("t"))), ScanStep::Done));
    assert!(state.finish().truncated);

    let mut state = ScanState::default();
    let big = "x".repeat(SCAN_MAX_BYTES / 2 + 1);
    let events = vec![ScanKind::Memory(memory("a", "@boss:x", &big, 1)), ScanKind::Memory(memory("b", "@boss:x", &big, 1))];
    assert!(matches!(state.push(page(events, Some("t"))), ScanStep::Done));
    let scan = state.finish();
    assert!(scan.truncated && scan.events.len() == 1, "the event over the byte cap is dropped");

    let mut state = ScanState::default();
    assert!(matches!(state.push(page(vec![ScanKind::Other], Some("same"))), ScanStep::Next(_)));
    assert!(matches!(state.push(page(vec![ScanKind::Other], Some("same"))), ScanStep::Done));
    assert!(state.finish().truncated, "a repeated token cannot loop");
}

#[test]
fn reduce_applies_power_levels_revisions_and_tombstones() {
    let scan = TimelineScan {
        events: vec![
            memory("a", "@boss:x", "one", 1),
            memory("a", "@boss:x", "two", 2),
            memory("a", "@mallory:x", "forged", 3),
            memory("b", "@boss:x", "gone", 1),
            json!({"type":MEMORY_STATE_TYPE,"sender":"@boss:x","content":{"id":"b","rev":2,"deleted":true}}),
            json!({"type":"m.room.message","sender":"@boss:x","content":{"id":"c","text":"wrong type","rev":1}}),
            json!({"type":MEMORY_STATE_TYPE,"sender":"@boss:x","content":{"id":"","text":"no id","rev":1}}),
            memory("d", "@boss:x", &"y".repeat(MEMORY_TEXT_BYTES + 1), 1),
        ],
        unreadable: 2,
        ..Default::default()
    };
    let view = reduce_timeline(&scan, &boss());
    assert_eq!(view.entries.len(), 1);
    assert_eq!(view.entries[0].text, "two");
    assert_eq!((view.ignored, view.unreadable), (1, 2));
}

#[test]
fn revision_content_round_trips_through_reduce() {
    let entry = Entry { id: "m1".into(), text: "note".into(), author: "@a:x".into(), source_event: None, pinned: true, rev: 4 };
    let live = TimelineScan { events: vec![json!({"type":MEMORY_STATE_TYPE,"sender":"@boss:x","content":revision_content(&entry,false)})], ..Default::default() };
    assert_eq!(reduce_timeline(&live, &boss()).entries, vec![entry.clone()]);
    let mut gone = entry;
    gone.rev = 5;
    let dead = TimelineScan { events: vec![live.events[0].clone(), json!({"type":MEMORY_STATE_TYPE,"sender":"@boss:x","content":revision_content(&gone,true)})], ..Default::default() };
    assert!(reduce_timeline(&dead, &boss()).entries.is_empty());
}
