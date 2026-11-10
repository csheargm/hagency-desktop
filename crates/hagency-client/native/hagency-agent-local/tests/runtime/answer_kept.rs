use super::*;
use hagency_agent_local::run_controls::{self, Action, Outcome};
use std::time::Duration;

/// The owner's ledger rows for the one admitted call, read through a second
/// connection so the assertions do not depend on the outcome under test.
#[derive(Debug, PartialEq)]
struct Rows {
    calls: Vec<(String, String, Option<String>)>,
    sessions: Vec<(String, String)>,
    codex: Vec<String>,
    accounts: Vec<(u64, u64)>,
}
fn rows(f: &Fixture) -> Rows {
    let db = rusqlite::Connection::open(f.path("ledger")).unwrap();
    fn all<T>(
        db: &rusqlite::Connection,
        sql: &str,
        row: impl Fn(&rusqlite::Row) -> rusqlite::Result<T>,
    ) -> Vec<T> {
        db.prepare(sql)
            .unwrap()
            .query_map([], |r| row(r))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
    Rows {
        calls: all(&db, "SELECT id,state,usage FROM calls ORDER BY id", |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        }),
        sessions: all(
            &db,
            "SELECT session,counters FROM external_sessions ORDER BY session",
            |r| Ok((r.get(0)?, r.get(1)?)),
        ),
        codex: all(&db, "SELECT counters FROM codex_usage ORDER BY scope", |r| {
            r.get(0)
        }),
        accounts: all(&db, "SELECT spent,held FROM accounts ORDER BY scope", |r| {
            Ok((r.get(0)?, r.get(1)?))
        }),
    }
}
/// What a failed native stream leaves in the ledger today: the call is
/// `unknown` with no usage, nothing is spent or held, and the session keeps
/// its identity while its cumulative counters are marked missing.
fn assert_unknown_usage(f: &Fixture, mode: &str) {
    let ledger = rows(f);
    assert_eq!(
        ledger.calls,
        [("first".to_owned(), "unknown".to_owned(), None)],
        "{mode}: the call stays unknown"
    );
    assert_eq!(
        ledger.sessions,
        [("offline-session".to_owned(), "null".to_owned())],
        "{mode}: the session row is kept and its counters are marked missing"
    );
    assert!(
        !ledger.codex.is_empty() && ledger.codex.iter().all(|counters| counters == "null"),
        "{mode}: the cumulative baseline is marked missing: {:?}",
        ledger.codex
    );
    assert!(
        ledger.accounts.iter().all(|account| *account == (0, 0)),
        "{mode}: nothing is charged or held: {:?}",
        ledger.accounts
    );
    assert_eq!(f.spent(), (0, 0), "{mode}");
}

/// F6 (a), (b) and the read error before idle: the stream fails only after
/// the final answer has been read. The answer is delivered; the ledger is
/// what the same failure left before, and the process is never reused.
#[tokio::test]
async fn claude_answer_already_read_is_delivered_when_the_stream_fails_afterwards() {
    let mut dropped = Vec::new();
    for mode in [
        "background_oversized",
        "background_exit",
        "background_fatal_frame",
        "result_exit",
    ] {
        let mut f = Fixture::new(Kind::Claude, mode, ToolPolicy::Deny);
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (queue, _) = codex::approval_queue(1).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(20),
            process.session.run_with_started(
                &mut f.ledger,
                "@owner:test",
                &f.scope,
                "first",
                "dispatch",
                "hello",
                BudgetMode::Postpaid,
                &queue,
                None,
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("{mode}: the failed stream ends the turn promptly"));
        assert_unknown_usage(&f, mode);
        let reply = match result {
            Ok(reply) => reply,
            Err(error) => {
                dropped.push(format!("{mode}: {error:?}"));
                process.stop().await.unwrap();
                continue;
            }
        };
        assert_eq!(reply.text, "offline reply", "{mode}");
        assert_eq!(reply.status, codex::TurnStatus::Completed, "{mode}");
        assert!(
            !reply.usage.is_reported(),
            "{mode}: usage stays unreported, as for any failed stream"
        );
        assert!(
            !process.session.can_continue(),
            "{mode}: a failed native process is never reused"
        );
        process.stop().await.unwrap();
    }
    assert!(
        dropped.is_empty(),
        "the answer that was already read was dropped: {dropped:?}"
    );
}

/// Only a clean final answer is kept. An error result, or text that arrived
/// under an accepted stop, still fails with the stream exactly as before.
#[tokio::test]
async fn claude_error_or_stopped_result_is_not_kept_when_the_stream_fails_afterwards() {
    let mut f = Fixture::new(Kind::Claude, "failed_exit", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        process.session.run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            "first",
            "dispatch",
            "hello",
            BudgetMode::Postpaid,
            &queue,
            None,
        ),
    )
    .await
    .unwrap();
    assert!(
        matches!(result, Err(Error::Protocol("Claude event"))),
        "an error result is not an answer: {result:?}"
    );
    assert_unknown_usage(&f, "failed_exit");
    process.stop().await.unwrap();

    let mut f = Fixture::new(
        Kind::Claude,
        "control_permission_success_exit",
        ToolPolicy::AskOwner,
    );
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (sender, inbox) = run_controls::channel(&f.scope, "dispatch", "first").unwrap();
    let (queue, mut approvals) = codex::approval_queue(1).unwrap();
    let queue = queue.with_run_controls(inbox);
    let run = process.session.run_with_started(
        &mut f.ledger,
        "@owner:test",
        &f.scope,
        "first",
        "dispatch",
        "hello",
        BudgetMode::Postpaid,
        &queue,
        None,
    );
    let host = async {
        let pending = approvals.recv().await.unwrap();
        let stop = sender.submit(Action::Interrupt).unwrap();
        assert_eq!(stop.receipt.await.unwrap(), Outcome::Accepted);
        while !pending.is_closed() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    };
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(run, host) })
            .await
            .unwrap();
    assert!(
        matches!(result, Err(Error::Protocol("Claude event"))),
        "text captured under an accepted stop is not a completed answer: {result:?}"
    );
    assert_unknown_usage(&f, "control_permission_success_exit");
    assert!(!f.path("effect").exists());
    process.stop().await.unwrap();
}

/// F6 (c): the owner stops the run after the final answer was read, while a
/// background child still waits for permission. The stop is honoured (the
/// child is never allowed) and the answer keeps its completed status and its
/// settled usage.
#[tokio::test]
async fn claude_stop_accepted_after_the_final_answer_still_completes_that_answer() {
    let mut f = Fixture::new(
        Kind::Claude,
        "background_stop_after_answer",
        ToolPolicy::AskOwner,
    );
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (sender, inbox) = run_controls::channel(&f.scope, "dispatch", "first").unwrap();
    let (queue, mut approvals) = codex::approval_queue(1).unwrap();
    let queue = queue.with_run_controls(inbox);
    let run = process.session.run_with_started(
        &mut f.ledger,
        "@owner:test",
        &f.scope,
        "first",
        "dispatch",
        "hello",
        BudgetMode::Postpaid,
        &queue,
        None,
    );
    let host = async {
        // The child's request follows the main answer on the native stream,
        // so the answer has been read before the stop is submitted.
        let pending = tokio::time::timeout(Duration::from_secs(10), approvals.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pending.proposal.tool, "claude.Bash");
        let stop = sender.submit(Action::Interrupt).unwrap();
        assert_eq!(stop.receipt.await.unwrap(), Outcome::Accepted);
        while !pending.is_closed() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    };
    let (done, ()) =
        tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(run, host) })
            .await
            .expect("the stopped run drains and settles");
    let done = done.unwrap();
    assert_eq!(done.text, "offline reply");
    assert!(!f.path("child-effect").exists(), "the stop withdrew the child");
    assert!(f.path("background-completed").exists());
    assert_eq!(done.usage.input + done.usage.output, 64);
    assert_eq!(f.spent(), (64, 0), "the stopped run settles its real usage");
    assert_eq!(
        done.status,
        codex::TurnStatus::Completed,
        "a stop accepted after the final answer does not turn it into an interrupted run"
    );
    process.stop().await.unwrap();
}
