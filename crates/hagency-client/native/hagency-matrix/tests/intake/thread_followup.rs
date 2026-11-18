//! The desktop composer's thread follow-up (`src/room/thread_followup.rs` in
//! the app): in the requester's one-Agent thread, a plain reply leaves the
//! composer with the Agent in `m.mentions` and no `@name` in the body. That
//! mention alone must wake the Agent; the same reply without it must not.
use super::*;

async fn followup(mentions: &[&str]) -> (IntakeSummary, Vec<bool>) {
    let f = common::Fixture::new();
    let mut fake = common::Fake::start(false).await;
    let c = Collector::new(
        config(&f, &fake.endpoint, f.identity.clone(), 1, false),
        f.store.clone(),
    )
    .unwrap();
    prime(&c, &f, &mut fake, false).await;
    let question = event("question", "@worker what are the issues on repo", &["@worker:example.test"], None);
    assert_eq!(run(&c, &mut fake, sync("question", vec![question]), false).await.unwrap().admitted, 1);
    let reply = event("followup", "are you the watcher", mentions, Some("$question"));
    let result = run(&c, &mut fake, sync("followup", vec![reply]), false).await.unwrap();
    let inbox = f.store.inbox("root".into(), 0, 10, None).await.unwrap();
    let wakes = inbox.iter().map(|i| i.wake).collect();
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
    (result, wakes)
}

#[tokio::test]
async fn native_matrix_thread_followup_mention_only_in_m_mentions_wakes() {
    let (result, wakes) = followup(&["@worker:example.test"]).await;
    assert_eq!(result.admitted, 1, "the composer's follow-up mention is admitted");
    assert_eq!(wakes, [true, true], "and wakes the Agent though the body names nobody");
}

#[tokio::test]
async fn native_matrix_thread_followup_without_mention_stays_quiet() {
    let (result, wakes) = followup(&[]).await;
    assert_eq!((result.admitted, result.filtered), (0, 1), "a plain thread reply still addresses nobody");
    assert_eq!(wakes, [true]);
}
