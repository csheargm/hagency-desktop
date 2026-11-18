//! User flow (live 2026-11-17, ADR 0023): Anna asks bird-codex, Betty's
//! Agent that does NOT watch the repository, to review PR #32 on the
//! sandbox that bird-claude watches for the dev room. The read runs without
//! a card; so does one on a repository linked to the room. A repository
//! outside the room's set, another room, or a requester the host did not
//! record as a member still ask.
use super::{TrustedStep, room_scope};
use crate::merge_policy::work::OwnerRules;
use crate::{Layer, Ledger, Scope, ToolPolicy, ToolProposal};
use serde_json::json;

const OWNER: &str = "@betty:example.org";
const ROOM: &str = "!dev-scope:example.org";
const REPO: &str = "csheargm/hagency-demo-sandbox";

fn proposal(agent: &str, requester: &str, room: &str, line: &str) -> ToolProposal {
    ToolProposal {
        scope: Scope { agent: agent.into(), binding: "b".into(), room: room.into(), requester: requester.into(), thread: "t".into() },
        dispatch: "d".into(), tool: "claude.Bash".into(), arguments: json!({"command": line}),
        canonical_directory: "/tmp".into(), risk: "high".into(), policy_revision: [1, 1, 1], expires: 100,
    }
}

fn ledger(dir: &std::path::Path, scopes: &[Scope]) -> Ledger {
    let mut ledger = Ledger::open(dir.join("local.db"), OWNER).unwrap();
    let deny = crate::Policy {
        budget: crate::Budget { limit: crate::Limit::Tokens(1000), period: crate::Period::Lifetime },
        requests: crate::RequestPolicy::Allow,
        high_risk: ToolPolicy::Deny,
    };
    for (i, scope) in scopes.iter().enumerate() {
        ledger.register_binding(OWNER, scope).unwrap();
        // The Agent and Room layers are shared; each requester has its own.
        let layers: &[Layer] = if i == 0 { &[Layer::Agent, Layer::Room, Layer::Requester] } else { &[Layer::Requester] };
        for layer in layers {
            ledger.set_policy(OWNER, scope, layer.clone(), 0, &deny).unwrap();
        }
        if i == 0 {
            ledger.reserve(scope, "call", "d", 10, 10).unwrap();
        }
    }
    ledger
}

fn runs(ledger: &mut Ledger, p: &ToolProposal) -> bool {
    matches!(ledger.trusted_publish(OWNER, p, 11).unwrap(), Some(TrustedStep::Command(_)))
}

#[test]
fn a_non_watching_agent_reads_the_rooms_watched_repo_without_a_card() {
    let db = tempfile::tempdir().unwrap();
    let pr = proposal("codex", "@anna:example.org", ROOM, "gh pr view 32 -R csheargm/hagency-demo-sandbox");
    let mut ledger = ledger(db.path(), &[pr.scope.clone()]);
    assert!(!runs(&mut ledger, &pr), "nothing watched for the room yet: Deny holds");
    ledger.set_work(OWNER, "bird-claude", vec![OwnerRules { repo: REPO.into(), room_id: ROOM.into(), ..Default::default() }]).unwrap();
    assert!(runs(&mut ledger, &pr), "bird-claude's watch puts the repository in the room's set");
    assert!(runs(&mut ledger, &proposal("codex", "@anna:example.org", ROOM, "gh pr diff 32 -R csheargm/hagency-demo-sandbox")));
    for asks in [
        proposal("codex", "@anna:example.org", ROOM, "gh pr merge 32 -R csheargm/hagency-demo-sandbox"),
        proposal("codex", "@anna:example.org", ROOM, "gh pr view 32 -R other/repo"),
        proposal("codex", "@anna:example.org", "!other-scope:example.org", "gh pr view 32 -R csheargm/hagency-demo-sandbox"),
    ] {
        assert!(!runs(&mut ledger, &asks), "{}", asks.arguments["command"]);
    }
}

#[test]
fn linked_repositories_count_only_for_a_recorded_member_requester() {
    let db = tempfile::tempdir().unwrap();
    let anna = proposal("codex", "@anna:example.org", ROOM, "gh issue list -R hagency-org/hagency-desktop");
    let eve = proposal("codex", "@eve:example.org", ROOM, "gh issue list -R hagency-org/hagency-desktop");
    let mut ledger = ledger(db.path(), &[anna.scope.clone(), eve.scope.clone()]);
    assert!(!runs(&mut ledger, &anna), "not linked yet");
    room_scope::record(ROOM, "@anna:example.org", vec!["hagency-org/hagency-desktop".into()]);
    assert!(runs(&mut ledger, &anna));
    assert!(!runs(&mut ledger, &eve), "a requester the host did not find in the room still asks");
    room_scope::record(ROOM, "@anna:example.org", Vec::new());
    assert!(!runs(&mut ledger, &anna), "an unlink ends it on the next turn's record");
}
