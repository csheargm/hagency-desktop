//! F5: a saved native session is retired only when the native CLI reports that
//! context missing. Any other failed startup of a resumed launch keeps the
//! saved row and returns the failure, so that the next message resumes it.
use super::*;

fn saved_sessions(f: &Fixture) -> Vec<String> {
    let db = rusqlite::Connection::open(f.path("ledger")).unwrap();
    let mut rows = db
        .prepare("SELECT session FROM external_sessions ORDER BY session")
        .unwrap();
    rows.query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}
fn launches(f: &Fixture) -> Vec<Vec<String>> {
    fs::read_to_string(f.path("argv"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn resumes(launch: &[String], session: &str) -> bool {
    launch
        .windows(2)
        .any(|pair| pair[0] == "--resume" && pair[1] == session)
}
fn prompts(f: &Fixture) -> Vec<String> {
    fs::read_to_string(f.path("frames"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|frame| frame["type"] == "user")
        .map(|frame| frame["message"]["content"].as_str().unwrap().to_owned())
        .collect()
}
async fn call(f: &mut Fixture, name: &str) {
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let reply = process
        .session
        .run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            name,
            name,
            name,
            BudgetMode::Postpaid,
            &queue,
            None,
        )
        .await
        .unwrap();
    assert!(reply.usage.is_reported());
    process.stop().await.unwrap();
}

#[tokio::test]
async fn claude_startup_failure_without_the_missing_context_diagnostic_keeps_the_saved_session() {
    let guardian = PathBuf::from(env!("CARGO_BIN_EXE_hagency-local-probe"));
    let mut discarded = Vec::new();
    for failure in ["reworded", "eof", "refused", "unexpected"] {
        let mut f = Fixture::new(Kind::Claude, "normal", ToolPolicy::Deny);
        call(&mut f, "before_failure").await;
        let saved = saved_sessions(&f);
        assert_eq!(saved.len(), 1, "{failure}");
        fs::write(f.path("missing-context"), failure).unwrap();
        let started = spawn_for_scope_with_guardian(
            &f.profile,
            &f.reference,
            &mut f.ledger,
            &f.scope,
            Some(&guardian),
        )
        .await;
        match started {
            Ok(mut process) => {
                discarded.push(format!(
                    "{failure}: fresh process returned, saved rows now {:?}",
                    saved_sessions(&f)
                ));
                process.stop().await.unwrap();
                continue;
            }
            Err(error) => assert!(
                matches!(error, Error::Protocol(text) if text.starts_with("Claude resume ")),
                "{failure}: {error:?}"
            ),
        }
        assert_eq!(saved_sessions(&f), saved, "{failure}");
        let launched = launches(&f);
        assert_eq!(
            launched.len(),
            2,
            "no fresh process replaces the failed resume: {failure}"
        );
        assert!(resumes(&launched[1], &saved[0]), "{failure}");
        assert_eq!(prompts(&f), ["before_failure"], "{failure}");
        // The next message resumes the same native session.
        fs::remove_file(f.path("missing-context")).unwrap();
        call(&mut f, "after_failure").await;
        let launched = launches(&f);
        assert_eq!(launched.len(), 3, "{failure}");
        assert!(resumes(&launched[2], &saved[0]), "{failure}");
        assert_eq!(saved_sessions(&f), saved, "{failure}");
        assert_eq!(prompts(&f), ["before_failure", "after_failure"], "{failure}");
        assert_eq!(f.spent(), (48, 0), "{failure}");
    }
    assert!(
        discarded.is_empty(),
        "a startup failure that does not report the context missing discarded the saved session: {discarded:?}"
    );
}

#[tokio::test]
async fn claude_missing_context_discard_is_reported_to_the_host_with_a_stable_code() {
    let guardian = PathBuf::from(env!("CARGO_BIN_EXE_hagency-local-probe"));
    let mut f = Fixture::new(Kind::Claude, "normal", ToolPolicy::Deny);
    call(&mut f, "before_missing").await;
    let saved = saved_sessions(&f);
    // An ordinary resume retires nothing.
    let mut resumed = spawn_for_scope_with_guardian(
        &f.profile,
        &f.reference,
        &mut f.ledger,
        &f.scope,
        Some(&guardian),
    )
    .await
    .unwrap();
    assert_eq!(resumed.discarded_session, None);
    resumed.stop().await.unwrap();
    fs::write(f.path("missing-context"), "deleted").unwrap();
    let mut fresh = spawn_for_scope_with_guardian(
        &f.profile,
        &f.reference,
        &mut f.ledger,
        &f.scope,
        Some(&guardian),
    )
    .await
    .unwrap();
    assert_eq!(
        fresh.discarded_session,
        Some(hagency_agent_local::runtime::SESSION_DISCARDED_MISSING)
    );
    assert_eq!(fresh.discarded_session, Some("native_context_missing"));
    assert!(saved_sessions(&f).is_empty(), "the missing session {saved:?} is retired");
    // The fresh process has started once it answers initialization.
    fresh.session.initialize().await.unwrap();
    let launched = launches(&f);
    assert_eq!(launched.len(), 4);
    assert!(resumes(&launched[2], &saved[0]));
    assert!(!launched[3].iter().any(|a| a == "--resume"));
    fresh.stop().await.unwrap();
}

// F5b: a saved session whose resumed start keeps failing before any input,
// without the missing-context diagnostic, is replaced by a fresh one on the
// third consecutive failure, and only once that fresh process has initialized.

async fn start(f: &mut Fixture) -> codex::Result<Process> {
    spawn_for_scope_with_guardian(
        &f.profile,
        &f.reference,
        &mut f.ledger,
        &f.scope,
        Some(&PathBuf::from(env!("CARGO_BIN_EXE_hagency-local-probe"))),
    )
    .await
}
/// One failed resumed start: the failure is returned and nothing is retired.
async fn fails_and_keeps(f: &mut Fixture, saved: &[String], step: &str) {
    match start(f).await {
        Ok(mut process) => {
            let reason = process.discarded_session;
            process.stop().await.unwrap();
            panic!("{step}: a process was returned (discard reason {reason:?})");
        }
        Err(error) => assert!(
            matches!(error, Error::Protocol(text) if text.starts_with("Claude resume ")),
            "{step}: {error:?}"
        ),
    }
    assert_eq!(saved_sessions(f), saved, "{step}");
}
async fn run_in(f: &mut Fixture, process: &mut Process, name: &str) {
    process.session.initialize().await.unwrap();
    process.session.verify_host_environment().await.unwrap();
    process.session.require_local_account().await.unwrap();
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let reply = process
        .session
        .run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            name,
            name,
            name,
            BudgetMode::Postpaid,
            &queue,
            None,
        )
        .await
        .unwrap();
    assert!(reply.usage.is_reported());
}

#[tokio::test]
async fn claude_third_consecutive_resume_failure_continues_in_a_fresh_session() {
    for failure in ["unexpected", "eof"] {
        let mut f = Fixture::new(Kind::Claude, "normal", ToolPolicy::Deny);
        call(&mut f, "before_failure").await;
        let saved = saved_sessions(&f);
        assert_eq!(saved.len(), 1, "{failure}");
        fs::write(f.path("missing-context"), failure).unwrap();
        fails_and_keeps(&mut f, &saved, &format!("{failure} first")).await;
        fails_and_keeps(&mut f, &saved, &format!("{failure} second")).await;
        let mut fresh = match start(&mut f).await {
            Ok(process) => process,
            Err(error) => panic!(
                "the third consecutive failed resume ({failure}) still failed the message: {error:?}"
            ),
        };
        assert_eq!(
            fresh.discarded_session,
            Some("native_resume_failed_repeatedly"),
            "{failure}"
        );
        assert!(saved_sessions(&f).is_empty(), "{failure}: the saved row is retired");
        let launched = launches(&f);
        assert_eq!(launched.len(), 5, "{failure}");
        for resumed in &launched[1..4] {
            assert!(resumes(resumed, &saved[0]), "{failure}");
        }
        assert!(!launched[4].iter().any(|a| a == "--resume"), "{failure}");
        // No prompt reached any failed process; this turn runs in the fresh one.
        assert_eq!(prompts(&f), ["before_failure"], "{failure}");
        run_in(&mut f, &mut fresh, "after_failure").await;
        fresh.stop().await.unwrap();
        assert_eq!(prompts(&f), ["before_failure", "after_failure"], "{failure}");
        assert_eq!(saved_sessions(&f).len(), 1, "{failure}: the fresh session is saved");
        assert_eq!(launches(&f).len(), 5, "{failure}");
        assert_eq!(f.spent(), (48, 0), "{failure}");
        // The count ended with the discard: the next failure is a first one.
        let saved = saved_sessions(&f);
        fails_and_keeps(&mut f, &saved, &format!("{failure} after the fresh start")).await;
    }
}

#[tokio::test]
async fn claude_fresh_start_that_also_fails_keeps_the_saved_session_and_the_count() {
    let mut f = Fixture::new(Kind::Claude, "normal", ToolPolicy::Deny);
    call(&mut f, "before_failure").await;
    let saved = saved_sessions(&f);
    fs::write(f.path("missing-context"), "eof").unwrap();
    fs::write(f.path("fresh-start-fails"), "").unwrap();
    fails_and_keeps(&mut f, &saved, "first").await;
    fails_and_keeps(&mut f, &saved, "second").await;
    assert_eq!(launches(&f).len(), 3);
    // Third: the fresh process is tried, fails to initialize, and retires nothing.
    let error = match start(&mut f).await {
        Ok(_) => panic!("a fresh process that cannot initialize was returned"),
        Err(error) => error,
    };
    assert!(matches!(error, Error::Protocol("Claude initialize")), "{error:?}");
    assert_eq!(saved_sessions(&f), saved);
    let launched = launches(&f);
    assert_eq!(launched.len(), 5, "the fresh start was attempted once");
    assert!(resumes(&launched[3], &saved[0]));
    assert!(!launched[4].iter().any(|a| a == "--resume"));
    // The count is kept: the next message tries the fresh start again.
    fs::remove_file(f.path("fresh-start-fails")).unwrap();
    let mut fresh = start(&mut f).await.unwrap();
    assert_eq!(fresh.discarded_session, Some("native_resume_failed_repeatedly"));
    assert!(saved_sessions(&f).is_empty());
    assert_eq!(launches(&f).len(), 7);
    assert_eq!(prompts(&f), ["before_failure"]);
    fresh.stop().await.unwrap();
}

#[tokio::test]
async fn claude_successful_resume_between_failures_resets_the_count() {
    let mut f = Fixture::new(Kind::Claude, "normal", ToolPolicy::Deny);
    call(&mut f, "before_failure").await;
    let saved = saved_sessions(&f);
    fs::write(f.path("missing-context"), "unexpected").unwrap();
    fails_and_keeps(&mut f, &saved, "first").await;
    fails_and_keeps(&mut f, &saved, "second").await;
    fs::remove_file(f.path("missing-context")).unwrap();
    let mut resumed = start(&mut f).await.unwrap();
    assert_eq!(resumed.discarded_session, None);
    assert!(resumes(launches(&f).last().unwrap(), &saved[0]));
    resumed.stop().await.unwrap();
    assert_eq!(saved_sessions(&f), saved);
    // Two more failures are again a first and a second one.
    fs::write(f.path("missing-context"), "unexpected").unwrap();
    fails_and_keeps(&mut f, &saved, "first after a successful resume").await;
    fails_and_keeps(&mut f, &saved, "second after a successful resume").await;
    assert!(launches(&f)[1..].iter().all(|launch| resumes(launch, &saved[0])));
    // And the third in a row is the one that starts fresh.
    let mut fresh = start(&mut f).await.expect("third consecutive failure starts fresh");
    assert_eq!(fresh.discarded_session, Some("native_resume_failed_repeatedly"));
    fresh.stop().await.unwrap();
}

#[tokio::test]
async fn claude_unsupported_version_never_counts_towards_a_fresh_start() {
    let mut f = Fixture::new(Kind::Claude, "normal", ToolPolicy::Deny);
    call(&mut f, "before_upgrade").await;
    let saved = saved_sessions(&f);
    fs::write(f.path("cli-version"), "2.1.321").unwrap();
    for attempt in 0..5 {
        let error = match start(&mut f).await {
            Ok(_) => panic!("attempt {attempt}: an unsupported installation started a process"),
            Err(error) => error,
        };
        assert!(
            matches!(error, Error::Protocol(text) if text.contains("2.1.322")),
            "attempt {attempt}: {error:?}"
        );
        assert_eq!(saved_sessions(&f), saved, "attempt {attempt}");
    }
    let launched = launches(&f);
    assert_eq!(launched.len(), 6);
    assert!(launched[1..].iter().all(|launch| resumes(launch, &saved[0])));
    // A supported installation then resumes the same session at once.
    fs::write(f.path("cli-version"), "2.1.322").unwrap();
    let mut resumed = start(&mut f).await.unwrap();
    assert_eq!(resumed.discarded_session, None);
    resumed.stop().await.unwrap();
    // Mixed in with real failures, it still does not count.
    fs::write(f.path("missing-context"), "eof").unwrap();
    fails_and_keeps(&mut f, &saved, "first").await;
    fs::write(f.path("cli-version"), "2.1.321").unwrap();
    fs::remove_file(f.path("missing-context")).unwrap();
    assert!(start(&mut f).await.is_err());
    fs::write(f.path("cli-version"), "2.1.322").unwrap();
    fs::write(f.path("missing-context"), "eof").unwrap();
    fails_and_keeps(&mut f, &saved, "second, after an unsupported version in between").await;
}
