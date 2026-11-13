use super::*;
use hagency_platform::Launch;
use hagency_runtime::{
    octos::session::{Event, Limits, Permissions},
    owned::OwnedOctosSession,
};
use std::{collections::BTreeMap, path::Path};

async fn owned(root: &Path, name: &str, mode: &str) -> OwnedOctosSession {
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!(
            "hagency-execution-probe{}",
            std::env::consts::EXE_SUFFIX
        ));
    assert!(binary.is_file());
    let launch = Launch {
        executable: binary.clone(),
        directory: root.into(),
        arguments: vec!["fake-octos".into(), mode.into(), root.join(name).into()],
        environment: BTreeMap::from([("PATH".into(), "".into())]),
        require_crash_containment: false,
    };
    let mut runner = OwnedOctosSession::spawn(
        &binary,
        &launch,
        Limits {
            write_timeout_ms: 1000,
            event_wait_ms: 8000,
            lifetime_ms: 30_000,
        },
    )
    .unwrap();
    runner.hello().await.unwrap();
    runner
        .open(
            "coding:local:usage-fixture",
            "coding",
            root.to_str().unwrap(),
            Permissions::ReadOnly,
        )
        .await
        .unwrap();
    runner
        .start_turn(
            "0192f0c1-0000-7000-8000-000000000001",
            "offline usage attribution",
        )
        .await
        .unwrap();
    runner
}

#[tokio::test]
async fn native_owned_octos_usage_keeps_original_source_and_unknown_counters() {
    for mode in ["normal", "background", "unknown-usage"] {
        let mut f = Fixture::family("octos").await;
        let mut runner = owned(f.root.path(), "original", mode).await;
        f.run.attach(&runner);
        assert!(f.run.status().attached);
        loop {
            let event = runner.next().await.unwrap();
            if f.run.observe(runner.last_observation().unwrap()) {
                f.run.record_pending().await.unwrap();
            }
            if matches!(event, Event::Idle(_)) {
                break;
            }
        }
        assert!(f.run.status().closed && f.run.status().incomplete);
        assert_eq!(f.run.status().failure, None);
        let view = f.domain.usage_source(f.run.source.clone()).await.unwrap();
        let expected = match mode {
            "normal" => [Some(10), Some(7), Some(20), Some(30)],
            "background" => [Some(915), Some(610), Some(60), Some(90)],
            _ => [None; 4],
        };
        assert_eq!(
            [
                view.high_water.input,
                view.high_water.output,
                view.high_water.cache_read,
                view.high_water.cache_write
            ],
            expected
        );
        let observations: Vec<String> = f
            .sql()
            .prepare("SELECT observation FROM usage_receipts ORDER BY call_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(!observations.is_empty());
        assert!(
            observations
                .iter()
                .all(|row| row.contains("octos_runtime_evidence") && !row.contains("coding:local"))
        );
        let source = runner.last_observation().unwrap().source().clone();
        runner.stop();
        assert!(source.is_retired());
        f.domain.shutdown().await.unwrap();
    }
    for mode in ["family", "foreign", "retired", "skipped"] {
        let mut f = Fixture::family(if mode == "family" { "codex" } else { "octos" }).await;
        let mut runner = owned(f.root.path(), "original", "normal").await;
        f.run.attach(&runner);
        let expected = match mode {
            "family" => UsageFailure::Source,
            "foreign" => {
                let mut other = owned(f.root.path(), "other", "normal").await;
                other.next().await.unwrap();
                assert!(!f.run.observe(other.last_observation().unwrap()));
                other.stop();
                UsageFailure::Source
            }
            "retired" => {
                runner.next().await.unwrap();
                let observed = runner.last_observation().unwrap().clone();
                runner.stop();
                assert!(!f.run.observe(&observed));
                UsageFailure::Retired
            }
            _ => {
                runner.next().await.unwrap();
                runner.next().await.unwrap();
                assert!(!f.run.observe(runner.last_observation().unwrap()));
                UsageFailure::Sequence
            }
        };
        assert_eq!(f.run.status().failure, Some(expected), "{mode}");
        runner.stop();
        f.domain.shutdown().await.unwrap();
    }
}
