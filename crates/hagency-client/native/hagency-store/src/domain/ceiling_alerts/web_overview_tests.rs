use super::*;
use crate::management::WebAlertsQuery;
use hagency_core::project::{Resource, Seat};

fn fixture() -> (tempfile::TempDir, DomainRepository, String, String) {
    let root = tempfile::tempdir().unwrap();
    let mut db =
        DomainRepository::open_managed(&root.path().join("state"), &"a".repeat(64)).unwrap();
    let mut ids = Vec::new();
    for (preset, profile) in [("visible", "a"), ("hidden", "b")] {
        let resource: Resource = serde_json::from_value(serde_json::json!({"presetId":preset,"seatId":preset,"framework":"codex","model":"fixture","published":false})).unwrap();
        db.put_seat(&Seat {
            id: preset.into(),
            declaration: None,
        })
        .unwrap();
        db.put_resource(&resource).unwrap();
        db.db
            .execute(
                "INSERT INTO owner_resource_grants VALUES(?1,?2,1,1)",
                params![profile.repeat(64), resource.id()],
            )
            .unwrap();
        ids.push(resource.id());
    }
    for n in 0..18 {
        db.db.execute("INSERT INTO ceiling_alerts(dedupe_key,resource_id,summary,detail,runbook,impact,recovery_condition,occurrences,first_seen_ms,last_seen_ms,status,severity) VALUES(?1,?2,'Observed','{\"secret\":\"private-provider-detail\"}','Review','Budget','Recheck',1,1,1,?3,?4)",params![format!("visible_{n:02}"),ids[0],if n==17 {"resolved"} else {"open"},if n==0 {"critical"} else {"warning"}]).unwrap();
    }
    db.db.execute("INSERT INTO ceiling_alerts(dedupe_key,resource_id,summary,detail,runbook,impact,recovery_condition,occurrences,first_seen_ms,last_seen_ms) VALUES('hidden_alert',?1,'Private profile','{}','r','i','c',1,1,1)",[&ids[1]]).unwrap();
    (root, db, ids.remove(0), ids.remove(0))
}
#[test]
fn web_alert_scope_counts_filters_pagination_and_revocation() {
    let (_root, db, visible, hidden) = fixture();
    let profile = "a".repeat(64);
    let before = db.db.total_changes();
    let first = db
        .management_web_alerts(Some(&profile), &WebAlertsQuery::default(), |_| Ok(()))
        .unwrap();
    assert_eq!(first.alerts.len(), 16);
    assert_eq!(first.counts.total, 18);
    assert_eq!(first.counts.by_status["open"], 17);
    assert_eq!(first.counts.open_by_severity["critical"], 1);
    assert_eq!(first.counts.open_by_severity["warning"], 16);
    assert!(
        !serde_json::to_string(&first)
            .unwrap()
            .contains("private-provider-detail")
    );
    // No readable figures: the display kind falls back instead of guessing.
    assert!(
        first
            .alerts
            .iter()
            .all(|a| a.kind == crate::ALERT_KIND_OTHER && a.params.is_empty())
    );
    let second = db
        .management_web_alerts(
            Some(&profile),
            &WebAlertsQuery {
                after: first.next_after,
                ..Default::default()
            },
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(second.alerts.len(), 2);
    assert!(second.next_after.is_none());
    let filtered = db
        .management_web_alerts(
            Some(&profile),
            &WebAlertsQuery {
                status: Some("open".into()),
                severity: Some("critical".into()),
                agent: Some(visible.clone()),
                ..Default::default()
            },
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(filtered.filtered_count, 1);
    assert_eq!(
        filtered.counts.total, 18,
        "filters never alter whole-scope counters"
    );
    let hidden = db
        .management_web_alerts(
            Some(&profile),
            &WebAlertsQuery {
                agent: Some(hidden),
                ..Default::default()
            },
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(hidden.filtered_count, 0);
    assert_eq!(
        db.db.total_changes(),
        before,
        "overview reads have no durable effect"
    );
    db.db
        .execute(
            "UPDATE owner_resource_grants SET active=0 WHERE profile=?1",
            [profile.clone()],
        )
        .unwrap();
    let revoked = db
        .management_web_alerts(Some(&profile), &WebAlertsQuery::default(), |_| Ok(()))
        .unwrap();
    assert_eq!(revoked.counts.total, 0);
    assert!(revoked.alerts.is_empty());
}
