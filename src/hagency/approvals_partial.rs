//! A partial approvals reading: the management list or some of this app's
//! own Agent queues did not answer. What they held last time is kept, not
//! dropped (integration review): replacing the whole list hid those cards
//! during the outage, and a request that expired meanwhile left no
//! "Expired, not answered" record because it was no longer known.
use super::local_approvals::{MANAGEMENT_ERROR, SOURCE, UNREADABLE};
use super::pending_approvals::{LocalDecision, PendingApproval};
use serde_json::Value;

/// Whether a reading `value` could not have seen `row`'s source: the
/// management part failed (for a management row), or the Agent's queue, or
/// every queue, was unreadable (for this app's own Agent's row).
pub fn unseen(value: &Value, row: &PendingApproval) -> bool {
    let local = &row.row[SOURCE];
    if local.is_null() {
        return !value[MANAGEMENT_ERROR].is_null();
    }
    let agent = local["agent"].as_str().unwrap_or_default();
    value[UNREADABLE].as_array().into_iter().flatten().any(|u| {
        let id = u["agentId"].as_str().unwrap_or_default();
        // A stale Agent list does not say which queues it read: keep every
        // own row it did not list (a newer reading may know an Agent it lacks).
        id == agent || id.is_empty()
    })
}

/// Split the previous rows into those this reading saw the source of and
/// those it did not.
pub fn split(value: &Value, previous: Vec<PendingApproval>) -> (Vec<PendingApproval>, Vec<PendingApproval>) {
    previous.into_iter().partition(|p| !unseen(value, p))
}

/// Keep the unseen rows: still open ones join `rows` (unless the reading
/// listed them after all); ones past their expiry, unanswered here, are
/// recorded as expired. Returns how many were recorded. If a later reading
/// shows someone did answer one, `pending_approvals` retracts the record.
pub fn keep(unseen: Vec<PendingApproval>, rows: &mut Vec<PendingApproval>, now: u64) -> usize {
    let mut expired = 0;
    for p in unseen {
        if rows.iter().any(|r| r.id() == p.id()) {
            continue;
        }
        if p.expires_at_ms().is_some_and(|t| t <= now) {
            let unanswered = matches!(p.local, LocalDecision::None | LocalDecision::Failed(..)) && p.can_decide();
            if unanswered && super::expired_approvals::record(&p.row, now) {
                expired += 1;
            }
            continue;
        }
        rows.push(p);
    }
    expired
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pending(id: &str, agent: Option<&str>, expires: u64) -> PendingApproval {
        let mut row = json!({"approval": {"id": id, "roomId": "!r:x", "expiresAtMs": expires}, "digest": "d", "decision": null, "canDecide": true});
        if let Some(agent) = agent {
            row[SOURCE] = json!({"agent": agent, "proposalId": "p"});
        }
        PendingApproval { row, local: LocalDecision::None }
    }

    #[test]
    fn only_the_sources_a_reading_missed_are_kept() {
        let _turn = super::super::pending_approvals::TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());
        super::super::expired_approvals::forget();
        let management_down = json!({"approvals": [], MANAGEMENT_ERROR: "approvals_read_timeout", UNREADABLE: []});
        let one_queue_down = json!({"approvals": [], UNREADABLE: [{"agent": "bird-claude", "agentId": "a1", "code": "approvals_read_timeout"}]});
        let all_queues_down = json!({"approvals": [], UNREADABLE: [{"agent": "", "agentId": "", "code": "approvals_read_timeout"}]});
        let stale_list = json!({"approvals": [], UNREADABLE: [{"agent": "", "agentId": "", "code": super::super::approvals_read::STALE_AGENT_LIST}]});
        let (sap, a1, a2) = (pending("sap", None, 10_000), pending("local:a1:p", Some("a1"), 10_000), pending("local:a2:p", Some("a2"), 10_000));
        assert!(unseen(&management_down, &sap) && !unseen(&management_down, &a1));
        assert!(unseen(&one_queue_down, &a1) && !unseen(&one_queue_down, &a2) && !unseen(&one_queue_down, &sap));
        assert!(unseen(&all_queues_down, &a1) && unseen(&all_queues_down, &a2) && !unseen(&all_queues_down, &sap));
        assert!(unseen(&stale_list, &a1), "a stale list may have missed a queue");

        // Management times out: its card stays; a local one read fresh is kept as read.
        let (seen, missed) = split(&management_down, vec![sap.clone(), a1.clone()]);
        assert_eq!((seen.len(), missed.len()), (1, 1));
        let mut rows = vec![a1.clone()];
        assert_eq!(keep(missed, &mut rows, 5_000), 0);
        assert_eq!(rows.iter().map(|p| p.id()).collect::<Vec<_>>(), vec!["local:a1:p", "sap"]);

        // It expires during the outage: recorded, not silently dropped.
        let mut rows = Vec::new();
        assert_eq!(keep(vec![sap.clone()], &mut rows, 10_000), 1);
        assert!(rows.is_empty());
        assert_eq!(super::super::expired_approvals::listed(10_000)[0].id(), "sap");
        // An answered one that expires is not "not answered".
        let mut sent = pending("sap2", None, 1);
        sent.local = LocalDecision::Sent("deny".into());
        assert_eq!(keep(vec![sent], &mut Vec::new(), 10_000), 0);
        super::super::expired_approvals::forget();
    }
}
